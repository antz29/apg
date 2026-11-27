use super::*;

/// unit tier -- pure in-memory: no filesystem, database, git or process.
mod unit {
    use super::*;
    use std::path::Path;
    use std::time::Duration;

    #[test]
    fn iso8601_formats_known_epochs() {
        assert_eq!(iso8601(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso8601(1_700_000_000), "2023-11-14T22:13:20Z");
        // The live formatter produces the same shape.
        let now = now_iso8601();
        assert_eq!(now.len(), "YYYY-MM-DDTHH:MM:SSZ".len());
        assert!(now.ends_with('Z'));
        assert!(now.as_bytes()[10] == b'T');
    }

    /// The refusal shapes (R3): every flavour names the current branch or
    /// checkout and carries exactly one `Fix:` line naming `apg project start`
    /// and the move into the project worktree. Pure: constructed identities.
    #[test]
    fn membership_refusal_names_branch_and_one_fix_line() {
        let identity =
            |branch: &str, default: &str, checkout: &str, is_worktree: bool| RepoIdentity {
                main_root: std::path::PathBuf::from("/repo"),
                checkout_root: std::path::PathBuf::from(checkout),
                is_worktree,
                branch: Some(branch.to_string()),
                default_branch: Some(default.to_string()),
                head_sha: Some("abc".into()),
            };
        let assert_fix = |msg: &str| {
            assert_eq!(msg.matches("Fix:").count(), 1, "one Fix line: {msg}");
            assert!(msg.starts_with("mutation refused — "), "{msg}");
            assert!(msg.contains("apg project start"), "{msg}");
            assert!(msg.contains("work inside its worktree"), "{msg}");
        };

        // Half 1, on the default branch: names `main` and the rule.
        let msg = format!(
            "{:#}",
            membership_refusal(&identity("main", "main", "/repo", false), "foo")
        );
        assert!(msg.contains("half 1 (branch)"), "{msg}");
        assert!(msg.contains("branch `main`"), "{msg}");
        assert!(
            msg.contains("default branch is never a mutation place"),
            "{msg}"
        );
        assert_fix(&msg);

        // Half 1, on another project branch: names both branches.
        let msg = format!(
            "{:#}",
            membership_refusal(
                &identity("bar", "main", "/repo/apg/.worktrees/bar", true),
                "foo"
            )
        );
        assert!(msg.contains("half 1 (branch)"), "{msg}");
        assert!(msg.contains("branch `bar`"), "{msg}");
        assert!(msg.contains("branch `foo`"), "{msg}");
        assert_fix(&msg);

        // Project-is-the-default: names the default branch and the rule.
        let msg = format!(
            "{:#}",
            membership_refusal(&identity("main", "main", "/repo", false), "main")
        );
        assert!(msg.contains("branch `main`"), "{msg}");
        assert!(
            msg.contains("default branch is never a mutation place"),
            "{msg}"
        );
        assert_fix(&msg);

        // Half 2: on the right branch, wrong checkout — names the branch and
        // both checkouts.
        let msg = format!(
            "{:#}",
            membership_refusal(
                &identity("foo", "main", "/repo/apg/.worktrees/bar", true),
                "foo"
            )
        );
        assert!(msg.contains("half 2 (worktree)"), "{msg}");
        assert!(msg.contains("branch `foo`"), "{msg}");
        assert!(msg.contains("/repo/apg/.worktrees/bar"), "{msg}");
        assert!(msg.contains("/repo/apg/.worktrees/foo"), "{msg}");
        assert_fix(&msg);
    }

    /// The pure ignore verdict (R1): paths are already workdir-relative, the
    /// tracked set and the ignore oracle are fakes, so every verdict is decided
    /// without git, filesystem or process I/O.
    #[test]
    fn decide_ignored_honours_tracked_and_empty_precedence() {
        use std::collections::HashSet;
        use std::path::{Path, PathBuf};

        // Fakes: a tracked set, and an ignore oracle that answers "ignored" for
        // exactly the two glob-matching inputs below.
        let tracked: HashSet<PathBuf> = HashSet::from([PathBuf::from("target/debug/app")]);
        let oracle = |rel: &Path| {
            rel == Path::new("target/debug/app") || rel == Path::new("node_modules/dep/index.js")
        };

        // 1. tracked_path_declined_despite_matching_ignore_glob: the oracle
        //    would say ignored, the tracked set wins.
        assert!(
            !decide_ignored(Path::new("target/debug/app"), &tracked, oracle),
            "tracked path must never be ignored, even when it matches an ignore glob"
        );

        // 2. untracked_ignored_path_ignored: the oracle decides.
        assert!(
            decide_ignored(Path::new("node_modules/dep/index.js"), &tracked, oracle),
            "an untracked path the rules ignore is ignored"
        );

        // 3. untracked_unignored_path_kept.
        assert!(
            !decide_ignored(Path::new("src/main.rs"), &tracked, oracle),
            "an untracked path the rules keep is kept"
        );

        // 4. empty_relative_path_kept: declined before the oracle is consulted.
        assert!(
            !decide_ignored(Path::new(""), &tracked, oracle),
            "an empty relative path is never ignored"
        );
    }

    /// The content-identity fold is a pure function of HEAD + entries: the same
    /// input twice yields the same `(clean, key)`, and the fold's own
    /// `(path, bits)` sort makes entry order irrelevant. Constructed values
    /// only — no repository, index or filesystem.
    #[test]
    fn fold_status_entries_is_deterministic_and_order_independent() {
        let entries = [
            StatusEntry {
                path: "src/b.rs".into(),
                bits: 0x0000_0100,
                index_oid: Some("bbbb".into()),
                content: Some(b"b".to_vec()),
            },
            StatusEntry {
                path: "src/a.rs".into(),
                bits: 0x0000_0001,
                index_oid: None,
                content: Some(b"a".to_vec()),
            },
            StatusEntry {
                path: "src/c.rs".into(),
                bits: 0x0000_0080,
                index_oid: None,
                content: None,
            },
        ];
        let first = fold_status_entries("deadbeef", &entries);
        assert_eq!(first, fold_status_entries("deadbeef", &entries));
        let mut shuffled = entries.clone();
        shuffled.reverse();
        assert_eq!(
            first,
            fold_status_entries("deadbeef", &shuffled),
            "the fold sorts by (path, bits)"
        );
        assert_eq!(first.1.len(), 16, "a 16-hex-digit key");
    }

    /// `clean` is exactly "the collection is empty": an empty slice is clean,
    /// and any entry at all is not, whatever its status bits or content.
    #[test]
    fn fold_status_entries_is_clean_only_when_empty() {
        assert!(fold_status_entries("deadbeef", &[]).0);
        let entry = StatusEntry {
            path: "src/lib.rs".into(),
            bits: 0,
            index_oid: None,
            content: None,
        };
        assert!(
            !fold_status_entries("deadbeef", &[entry]).0,
            "one entry is a change"
        );
    }

    /// Every folded field moves the key: the working-tree bytes, the status
    /// bits, and membership itself.
    #[test]
    fn fold_status_entries_key_flips_on_content_bits_and_membership() {
        let base = StatusEntry {
            path: "src/lib.rs".into(),
            bits: 0x0000_0100,
            index_oid: Some("0123abcd".into()),
            content: Some(b"one".to_vec()),
        };
        let key = |e: &StatusEntry| fold_status_entries("head", std::slice::from_ref(e)).1;

        let mut content_edit = base.clone();
        content_edit.content = Some(b"two".to_vec());
        assert_ne!(key(&base), key(&content_edit), "bytes change the key");

        let mut bits_edit = base.clone();
        bits_edit.bits = 0x0000_0200;
        assert_ne!(key(&base), key(&bits_edit), "status bits change the key");

        let (clean, with_extra) = fold_status_entries(
            "head",
            &[
                base.clone(),
                StatusEntry {
                    path: "src/new.rs".into(),
                    bits: 0x0000_0002,
                    index_oid: None,
                    content: None,
                },
            ],
        );
        assert!(!clean);
        assert_ne!(key(&base), with_extra, "an added entry changes the key");
        assert_ne!(
            fold_status_entries("head", &[]).1,
            key(&base),
            "a removed entry changes the key"
        );
    }

    /// Pins the digest byte-for-byte. Derivation: an independent reference
    /// FNV-1a 64 loop (seed `0xcbf2_9ce4_8422_2325`) over the HEAD sha, then
    /// the entries sorted by `(path, bits)` — `a.txt`, `b.txt`, `c.txt`,
    /// `d.txt` — each folding path bytes → status bits little-endian
    /// (`01000000`, `00010000`, `20000000`, `00800000`) → index-oid string →
    /// working-tree bytes, rendered `{h:016x}`. The entries cover `index_oid`
    /// `Some`/`None` and `content` `Some`/`None` with nonzero asymmetric bits.
    #[test]
    fn fold_status_entries_matches_the_pinned_digest() {
        let head = "0123456789abcdef0123456789abcdef01234567";
        let entries = [
            StatusEntry {
                path: "a.txt".into(),
                bits: 0x0000_0001,
                index_oid: Some("1111111111111111111111111111111111111111".into()),
                content: Some(b"alpha".to_vec()),
            },
            StatusEntry {
                path: "b.txt".into(),
                bits: 0x0000_0100,
                index_oid: Some("2222222222222222222222222222222222222222".into()),
                content: None,
            },
            StatusEntry {
                path: "c.txt".into(),
                bits: 0x0000_0020,
                index_oid: None,
                content: Some(b"gamma".to_vec()),
            },
            StatusEntry {
                path: "d.txt".into(),
                bits: 0x0000_8000,
                index_oid: None,
                content: None,
            },
        ];
        assert_eq!(
            fold_status_entries(head, &entries),
            (false, "88227b9c35351ba6".to_string()),
            "pinned fold over the fixed input"
        );
        let mut reversed = entries.clone();
        reversed.reverse();
        assert_eq!(
            fold_status_entries(head, &reversed).1,
            "88227b9c35351ba6",
            "the pinned digest is order-independent"
        );
        // The head-only key (the `statuses()`-error digest): seed + HEAD alone.
        assert_eq!(
            fold_status_entries(head, &[]),
            (true, "1b6e0c21d8c6042d".to_string()),
            "an empty collection folds to the head-only key"
        );
    }

    /// unit tier -- pure in-memory: `classify_freshness` folds constructed
    /// facts only (no git, filesystem, database or process). Pins that no
    /// database or no HEAD sha is `NotApplicable` carrying the precondition's
    /// labels through unchanged, an empty difference set is `Fresh`, and a
    /// non-empty one is `Stale` carrying exactly those labels in order.
    #[test]
    fn classify_freshness_maps_not_applicable_fresh_and_stale() {
        let current = || GitState {
            sha: Some("0123456789abcdef0123456789abcdef01234567".into()),
            clean: true,
            content_key: Some("0123456789abcdef".into()),
        };
        let recorded = || RecordedScan {
            sha: "0123456789abcdef0123456789abcdef01234567".into(),
            clean: true,
            content_key: Some("0123456789abcdef".into()),
            modules: String::new(),
        };

        // No database: N/A even with a captured HEAD sha; the precondition's
        // label passes through unchanged.
        assert_eq!(
            classify_freshness(current(), Some(recorded()), false, vec!["database"]),
            FreshnessSnapshot::NotApplicable {
                current: current(),
                recorded: Some(recorded()),
                differing: vec!["database"],
            }
        );
        // Database present but no HEAD sha (no repo / unborn HEAD): N/A.
        let unborn = GitState {
            sha: None,
            clean: false,
            content_key: None,
        };
        assert_eq!(
            classify_freshness(unborn.clone(), None, true, vec!["git sha"]),
            FreshnessSnapshot::NotApplicable {
                current: unborn,
                recorded: None,
                differing: vec!["git sha"],
            }
        );
        // Database + HEAD + nothing differing is Fresh.
        assert_eq!(
            classify_freshness(current(), Some(recorded()), true, vec![]),
            FreshnessSnapshot::Fresh {
                current: current(),
                recorded: Some(recorded()),
            }
        );
        // Database + HEAD + differences is Stale, labels exactly in order.
        assert_eq!(
            classify_freshness(current(), Some(recorded()), true, vec!["sha", "clean"]),
            FreshnessSnapshot::Stale {
                current: current(),
                recorded: Some(recorded()),
                differing: vec!["sha", "clean"],
            }
        );
    }

    /// unit tier -- pure in-memory: the freshness invariant is pinned on the
    /// constructed verdicts — an empty difference set can never be `Stale`
    /// (it is `Fresh`), and every `Stale` verdict carries a non-empty
    /// `differing` list, so a stale result always names a differing input.
    #[test]
    fn classify_freshness_stale_always_names_a_differing_input() {
        let current = GitState {
            sha: Some("0123456789abcdef0123456789abcdef01234567".into()),
            clean: true,
            content_key: Some("0123456789abcdef".into()),
        };
        assert!(matches!(
            classify_freshness(current.clone(), None, true, vec![]),
            FreshnessSnapshot::Fresh { .. }
        ));
        for labels in [
            vec!["sha"],
            vec!["sha", "clean"],
            vec!["sha", "clean", "content", "modules"],
        ] {
            match classify_freshness(current.clone(), None, true, labels.clone()) {
                FreshnessSnapshot::Stale { differing, .. } => {
                    assert!(
                        !differing.is_empty(),
                        "a stale verdict always names at least one differing input"
                    );
                    assert_eq!(differing, labels, "stale labels carry through in order");
                }
                other => panic!("expected Stale for {labels:?}, got {other:?}"),
            }
        }
    }

    /// A transient lock/busy failure is classified retryable; a terminal one is
    /// not. Both directions are asserted explicitly, and the wrapped case
    /// confirms the whole `{err:#}` chain is scanned.
    #[test]
    fn is_transient_read_error_classifies_lock_busy_and_terminal() {
        let transient = [
            anyhow::Error::msg("Could not set lock on file /repo/apg/.trans/db.lbug"),
            anyhow::Error::msg("resource temporarily unavailable"),
            anyhow::Error::msg("could not set lock on file db.lbug")
                .context("opening the database"),
        ];
        for err in transient {
            assert!(
                is_transient_read_error(&err),
                "expected transient for `{err:#}`"
            );
        }

        let terminal = [
            anyhow::Error::msg("no such table: Foo"),
            anyhow::Error::msg("clock skew detected"),
            anyhow::Error::msg("blocked by policy"),
            anyhow::Error::msg("some unrelated read error"),
        ];
        for err in terminal {
            assert!(
                !is_transient_read_error(&err),
                "expected terminal for `{err:#}`"
            );
        }
    }

    /// A reader failing transiently N times (well under the bound) then
    /// returning `Ok` retries exactly N times and succeeds; every injected
    /// sleep is one `FRESHNESS_READ_STEP`.
    #[test]
    fn freshness_read_retry_retries_transient_until_success() {
        let database = Path::new("/repo/apg/.trans/db.lbug");
        let mut reads = 0;
        let mut sleeps: Vec<Duration> = Vec::new();
        let result = freshness_read_retry(
            database,
            || {
                reads += 1;
                if reads <= 3 {
                    Err(anyhow::Error::msg("Could not set lock on file db.lbug"))
                } else {
                    Ok("row")
                }
            },
            |d| sleeps.push(d),
        );
        assert_eq!(result.unwrap(), "row");
        assert_eq!(reads, 4, "three transient failures then one success");
        assert_eq!(sleeps, vec![FRESHNESS_READ_STEP; 3], "one step per retry");
    }

    /// A terminal failure returns at once: no sleep, one read, and a
    /// `FreshnessReadFailure` whose `waited` is zero. Its `Display` names the
    /// database and the underlying cause, and claims no wait, budget or
    /// contention for a read that never retried.
    #[test]
    fn freshness_read_retry_does_not_retry_a_terminal_error() {
        let database = Path::new("/repo/apg/.trans/db.lbug");
        let mut reads = 0;
        let mut slept = false;
        let err = freshness_read_retry::<()>(
            database,
            || {
                reads += 1;
                Err(anyhow::Error::msg(
                    "Permission denied (os error 13) reading db.lbug",
                ))
            },
            |_| slept = true,
        )
        .unwrap_err();
        assert_eq!(reads, 1, "a terminal error is read once");
        assert!(!slept, "a terminal error never sleeps");
        let failure = err
            .downcast::<FreshnessReadFailure>()
            .expect("the error is a FreshnessReadFailure");
        assert_eq!(failure.waited, Duration::ZERO);
        assert_eq!(failure.budget, FRESHNESS_READ_BUDGET);
        let rendered = failure.to_string();
        assert!(
            rendered.contains("/repo/apg/.trans/db.lbug"),
            "Display names the database path: {rendered}"
        );
        assert!(
            rendered.contains("Permission denied (os error 13) reading db.lbug"),
            "Display names the underlying cause: {rendered}"
        );
        for claim in [
            "waited",
            "budget",
            "contention",
            "another process",
            "retry once the other process",
        ] {
            assert!(
                !rendered.contains(claim),
                "a terminal read makes no `{claim}` claim: {rendered}"
            );
        }
    }

    /// An always-transient reader stops at the budget with `waited == budget`,
    /// the path it was given, the last read error, and the exact read/sleep
    /// counts the constants imply; `Display` names the path and the budget.
    #[test]
    fn freshness_read_retry_exhausts_at_the_budget() {
        let database = Path::new("/repo/apg/.trans/db.lbug");
        let steps = (FRESHNESS_READ_BUDGET.as_millis() / FRESHNESS_READ_STEP.as_millis()) as usize;
        let mut reads = 0;
        let mut sleeps = 0usize;
        let err = freshness_read_retry::<()>(
            database,
            || {
                reads += 1;
                Err(anyhow::Error::msg("database is locked"))
            },
            |_| sleeps += 1,
        )
        .unwrap_err();
        let failure = err
            .downcast::<FreshnessReadFailure>()
            .expect("the error is a FreshnessReadFailure");
        assert_eq!(failure.budget, FRESHNESS_READ_BUDGET);
        assert_eq!(failure.waited, FRESHNESS_READ_BUDGET);
        assert_eq!(failure.database, database.to_path_buf());
        assert!(
            format!("{:#}", failure.source).contains("database is locked"),
            "the source carries the last read error"
        );
        assert_eq!(
            reads,
            steps + 1,
            "one final read discovers the exhausted budget"
        );
        assert_eq!(sleeps, steps, "one sleep per retry step the budget allows");
        let rendered = failure.to_string();
        assert!(
            rendered.contains("/repo/apg/.trans/db.lbug"),
            "Display names the database path: {rendered}"
        );
        assert!(
            rendered.contains("waited 500ms of a 500ms budget"),
            "Display names the waited/budget pair: {rendered}"
        );
        assert!(
            rendered.contains("contention"),
            "a retried read keeps the contention phrasing: {rendered}"
        );
        assert!(
            rendered.contains("database is locked"),
            "Display names the underlying cause: {rendered}"
        );
    }

    /// A transient failure followed by a terminal one stops on the terminal
    /// after exactly one step of wait.
    #[test]
    fn freshness_read_retry_stops_on_terminal_after_transient() {
        let database = Path::new("/repo/apg/.trans/db.lbug");
        let mut reads = 0;
        let mut sleeps: Vec<Duration> = Vec::new();
        let err = freshness_read_retry::<()>(
            database,
            || {
                reads += 1;
                if reads == 1 {
                    Err(anyhow::Error::msg("Could not set lock on file db.lbug"))
                } else {
                    Err(anyhow::Error::msg("no such table: Foo"))
                }
            },
            |d| sleeps.push(d),
        )
        .unwrap_err();
        assert_eq!(reads, 2);
        assert_eq!(sleeps, vec![FRESHNESS_READ_STEP]);
        let failure = err
            .downcast::<FreshnessReadFailure>()
            .expect("the error is a FreshnessReadFailure");
        assert_eq!(failure.waited, FRESHNESS_READ_STEP);
    }
}
