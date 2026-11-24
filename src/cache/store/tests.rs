//! Unit-tier tests for the fact store's in-memory surface — pure in-memory, no
//! database, filesystem, git or process.

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::*;

/// unit tier — pure in-memory assertions over the declared schema, the format
/// predicate and the writer-wait/contention reporting.
mod unit {
    use super::*;

    #[test]
    fn the_declared_table_set_is_the_named_schema() {
        let mut declared = table_names();
        declared.sort_unstable();
        let expected = vec![
            "bodies",
            "deps",
            "intents",
            "meta",
            "overloads",
            "refs",
            "scaffolding",
            "scans",
            "signatures",
            "units",
            "worktrees",
        ];
        assert_eq!(declared, expected);
        assert_eq!(declared.len(), 11);
    }

    #[test]
    fn a_missing_or_other_format_needs_a_rebuild() {
        assert!(format_needs_rebuild(None));
        assert!(!format_needs_rebuild(Some(FACTS_FORMAT_VERSION)));
        assert!(format_needs_rebuild(Some(FACTS_FORMAT_VERSION + 1)));
    }

    #[test]
    fn the_writer_wait_budget_bounds_the_retry_loop() {
        assert_eq!(WRITER_WAIT_BUDGET, Duration::from_secs(30));
        assert!(WRITER_WAIT_POLL > Duration::ZERO);
        assert!(WRITER_WAIT_POLL < WRITER_WAIT_BUDGET);
        assert!(
            WRITER_WAIT_BUDGET.as_millis() / WRITER_WAIT_POLL.as_millis() >= 2,
            "the loop must retry more than once inside its budget"
        );
        assert!(!writer_wait_exhausted(
            WRITER_WAIT_BUDGET - WRITER_WAIT_POLL
        ));
        assert!(writer_wait_exhausted(WRITER_WAIT_BUDGET));
        assert!(writer_wait_exhausted(WRITER_WAIT_BUDGET * 2));
    }

    #[test]
    fn the_writer_wait_budget_boundary_is_inclusive() {
        assert!(!writer_wait_exhausted(
            WRITER_WAIT_BUDGET - Duration::from_millis(1)
        ));
        assert!(writer_wait_exhausted(WRITER_WAIT_BUDGET));
        assert!(writer_wait_exhausted(
            WRITER_WAIT_BUDGET + Duration::from_millis(1)
        ));
    }

    #[test]
    fn the_lock_path_is_the_facts_lock_sibling_of_the_database() {
        assert_eq!(
            lock_path(Path::new("/root/apg/facts.db")),
            PathBuf::from("/root/apg/facts.lock")
        );
        assert_eq!(LOCK_FILE, "facts.lock");
        // Any database name in the directory maps to the one `facts.lock`
        // gate beside it.
        assert_eq!(
            lock_path(Path::new("/root/apg/store.db")),
            PathBuf::from("/root/apg/facts.lock")
        );
        assert_eq!(
            lock_path(Path::new("facts.db")),
            PathBuf::from("facts.lock")
        );
    }

    #[test]
    fn the_contention_error_names_the_path_the_contention_and_the_wait() {
        let message =
            contention_error(Path::new("/x/facts.db"), Duration::from_secs(30)).to_string();
        assert!(
            message.contains("/x/facts.db"),
            "unexpected error: {message}"
        );
        assert!(
            message.contains("held by another writer"),
            "unexpected error: {message}"
        );
        assert!(message.contains("30s"), "unexpected error: {message}");
        assert!(
            message.contains("write lock"),
            "unexpected error: {message}"
        );
    }

    #[test]
    fn the_contention_error_is_typed_while_an_ordinary_failure_is_not() {
        let err = contention_error(Path::new("/root/apg/facts.db"), Duration::from_secs(30));
        assert!(is_contention(&err), "the named contention must be typed");
        let contention = err
            .downcast_ref::<Contention>()
            .expect("the contention error downcasts to `Contention`");
        assert_eq!(contention.path, PathBuf::from("/root/apg/facts.db"));
        assert_eq!(contention.waited, Duration::from_secs(30));
        assert!(
            err.to_string()
                .contains(&format!("budget {WRITER_WAIT_BUDGET:?}")),
            "the message must name the budget"
        );
        assert!(
            !is_contention(&anyhow::anyhow!("an ordinary store failure")),
            "an ordinary failure must not read as contention"
        );
    }

    #[test]
    fn the_five_part_unit_key_carries_no_inputs_or_root() {
        let key = UnitKey {
            format: FACTS_FORMAT_VERSION,
            config_digest: "cfg-digest".into(),
            lang: "rust".into(),
            rel: "src/lib.rs".into(),
            oid: "deadbeef".into(),
        };
        let encoded = unit_key(&key).expect("encode the unit key");
        let decoded: UnitKey = from_bytes(&encoded).expect("decode the unit key");
        assert_eq!(decoded, key);
        // The diagnostic and the writing root live in the row, not the key.
        let row = UnitRow {
            body: [7u8; 32],
            inputs: "diagnostic".into(),
            writer_root: "/checkout".into(),
        };
        let row_bytes = to_bytes(&row).expect("encode the unit row");
        assert_eq!(from_bytes::<UnitRow>(&row_bytes).expect("decode"), row);
        assert!(
            !encoded
                .windows(row.inputs.len())
                .any(|window| window == row.inputs.as_bytes()),
            "the key must not embed the diagnostic inputs"
        );
    }

    #[test]
    fn the_scaffolding_key_round_trips() {
        let key = ScaffoldingKey {
            format: FACTS_FORMAT_VERSION,
            config_digest: "cfg-digest".into(),
            lang: "go".into(),
        };
        let encoded = scaffolding_key(&key).expect("encode the scaffolding key");
        assert_eq!(from_bytes::<ScaffoldingKey>(&encoded).expect("decode"), key);
    }

    #[test]
    fn a_refs_key_is_the_worktree_prefix_then_the_path() {
        let mut worktree = [0u8; 16];
        worktree[0] = 1;
        worktree[15] = 9;
        let key = ref_key(worktree, "src/main.rs");
        assert_eq!(&key[..16], &worktree);
        assert_eq!(&key[16..], b"src/main.rs");
    }
}
