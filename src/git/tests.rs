use super::*;

/// unit tier -- pure in-memory: no filesystem, database, git or process.
mod unit {
    use super::*;

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
}
