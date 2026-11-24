use std::path::Path;

use super::*;

/// unit tier — pure in-memory: exactly [`rebuild_catching_panic`] under test,
/// the rebuild faked by an in-memory closure; no filesystem, database, git or
/// process I/O. Likewise the cache-copy policy resolver and the CoW
/// capability-error classifier ([`resolve_policy`], [`is_cow_unsupported`]):
/// their inputs are plain `Option<&str>`s and `i32`s, so nothing here reads the
/// environment, the config or the filesystem.
mod unit {
    use super::*;

    #[test]
    fn resolve_policy_takes_the_first_present_source() {
        // CLI > environment > config > auto.
        assert_eq!(
            resolve_policy(Some("skip"), Some("copy"), Some("clone")).unwrap(),
            CacheCopyPolicy::Skip
        );
        assert_eq!(
            resolve_policy(None, Some("copy"), Some("clone")).unwrap(),
            CacheCopyPolicy::Copy
        );
        assert_eq!(
            resolve_policy(None, None, Some("clone")).unwrap(),
            CacheCopyPolicy::Clone
        );
        assert_eq!(
            resolve_policy(None, None, None).unwrap(),
            CacheCopyPolicy::Auto
        );
    }

    #[test]
    fn resolve_policy_trims_each_present_value() {
        assert_eq!(
            resolve_policy(Some("  copy  "), None, None).unwrap(),
            CacheCopyPolicy::Copy
        );
        // The env and config positions trim the same way.
        assert_eq!(
            resolve_policy(None, Some(" skip "), None).unwrap(),
            CacheCopyPolicy::Skip
        );
        assert_eq!(
            resolve_policy(None, None, Some("\tclone\n")).unwrap(),
            CacheCopyPolicy::Clone
        );
    }

    #[test]
    fn resolve_policy_parses_only_the_first_present_value() {
        // Only the highest-precedence present candidate is parsed, so a bogus
        // value in a lower-precedence source is irrelevant.
        assert_eq!(
            resolve_policy(Some("copy"), Some("bogus"), None).unwrap(),
            CacheCopyPolicy::Copy
        );
        assert_eq!(
            resolve_policy(Some("skip"), None, Some("also-bogus")).unwrap(),
            CacheCopyPolicy::Skip
        );
    }

    #[test]
    fn resolve_policy_errors_name_the_source_the_value_and_the_accepted_set() {
        // Each invalid present value errors, naming its source and the value;
        // the full context chain lists the accepted values.
        for (cli, env, config, source) in [
            (Some("bogus"), None, None, "--cache-copy"),
            (None, Some("bogus"), None, "APG_CACHE_COPY"),
            (
                None,
                None,
                Some("bogus"),
                "apg/config.json cache.project-start",
            ),
        ] {
            let err =
                resolve_policy(cli, env, config).expect_err("an invalid present value must error");
            let outer = err.to_string();
            assert!(outer.contains(source), "message was: {outer}");
            assert!(outer.contains("bogus"), "message was: {outer}");
            let full = format!("{err:#}");
            for accepted in ["auto", "clone", "copy", "skip"] {
                assert!(
                    full.contains(accepted),
                    "accepted `{accepted}` missing from: {full}"
                );
            }
        }
    }

    #[test]
    fn cache_copy_policy_parse_and_as_str_round_trip() {
        for policy in [
            CacheCopyPolicy::Auto,
            CacheCopyPolicy::Clone,
            CacheCopyPolicy::Copy,
            CacheCopyPolicy::Skip,
        ] {
            assert_eq!(
                CacheCopyPolicy::parse(policy.as_str()).unwrap(),
                policy,
                "round trip failed for {policy:?}"
            );
        }
        let err = CacheCopyPolicy::parse("sometimes").expect_err("an unknown token must error");
        assert!(err.to_string().contains("sometimes"), "message was: {err}");
    }

    /// The CoW capability-error set only. Whether a clone that reports one of
    /// these falls back to a copy under [`CacheCopyPolicy::Auto`] or fails as a
    /// named error under [`CacheCopyPolicy::Clone`] is [`materialise`]'s real
    /// I/O behaviour, covered by the e2e tier; this test asserts the pure
    /// classification alone.
    #[test]
    fn is_cow_unsupported_recognises_exactly_the_capability_errors() {
        for errno in [
            libc::ENOTSUP,
            libc::EOPNOTSUPP,
            libc::EXDEV,
            libc::ENOTTY,
            libc::EINVAL,
        ] {
            assert!(is_cow_unsupported(errno), "errno {errno} must fall back");
        }
        for errno in [libc::EACCES, libc::EIO, libc::ENOENT, 0] {
            assert!(
                !is_cow_unsupported(errno),
                "errno {errno} must not fall back"
            );
        }
    }

    #[test]
    fn rebuild_panic_is_reported_as_an_error() {
        let dir = Path::new("/nonexistent/main");

        // A panicking rebuild becomes an `Err` naming the panic, carrying the
        // payload — so it takes the same rollback path as a returned error.
        let panicking = |_: &Path| -> anyhow::Result<()> { panic!("boom") };
        let err = rebuild_catching_panic(dir, &panicking).expect_err("a panic must become Err");
        let msg = err.to_string();
        assert!(msg.contains("main rebuild panicked"), "message was: {msg}");
        assert!(msg.contains("boom"), "message was: {msg}");

        // A successful rebuild passes through.
        let ok = |_: &Path| -> anyhow::Result<()> { Ok(()) };
        assert!(rebuild_catching_panic(dir, &ok).is_ok());

        // A returned error passes through unchanged.
        let failing = |_: &Path| -> anyhow::Result<()> { Err(anyhow::anyhow!("returned failure")) };
        let err =
            rebuild_catching_panic(dir, &failing).expect_err("a returned Err must pass through");
        assert!(
            err.to_string().contains("returned failure"),
            "message was: {err}"
        );
    }
}
