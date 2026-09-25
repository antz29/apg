use super::*;

/// int tier -- pure in-memory: the seed-vs-full-load DECISION surface wired
/// through the extracted equivalence predicate; no filesystem, database, git
/// or process. The `db.lbug` copy/schema/DB-equality half stays e2e below.
mod int {
    use super::*;

    /// fix-module-identity phase-06 task-10: the seed guard's content-key
    /// equivalence (`seed_key_matches`) is the same rule `seed_checked`
    /// applies — equal keys both present is the ONLY eligible combination.
    #[test]
    fn stale_seed_content_key_equivalence_is_wired() {
        assert!(seed_key_matches(Some("k1"), Some("k1")));
        // A missing key on EITHER side is never equivalent.
        assert!(!seed_key_matches(None, Some("k1")));
        assert!(!seed_key_matches(Some("k1"), None));
        assert!(!seed_key_matches(None, None));
        // A drift is ineligible.
        assert!(!seed_key_matches(Some("k1"), Some("k2")));
    }

    /// Every `SeedFallback` variant describes itself for the scan log (the
    /// dispatch seam's one-line message), each naming the full-load outcome.
    #[test]
    fn every_seed_fallback_variant_describes_itself() {
        let cases = [
            (
                SeedFallback::MissingPrevious,
                "no previous db.lbug to seed from",
            ),
            (SeedFallback::Unreadable("corrupt".into()), "corrupt"),
            (
                SeedFallback::IncompatibleSchema("missing tables: X".into()),
                "missing tables: X",
            ),
            (
                SeedFallback::StaleSeed {
                    seed: "seed-key".into(),
                    recorded: "rec-key".into(),
                },
                "seed-key",
            ),
            (
                SeedFallback::SeededCopyUnreadable("bad copy".into()),
                "bad copy",
            ),
        ];
        for (fallback, needle) in &cases {
            let described = fallback.describe();
            assert!(
                described.contains(needle),
                "{fallback:?} describe() must carry {needle:?}: {described}"
            );
        }
        // The stale-seed line names BOTH keys so the log attributes the drift.
        let stale = SeedFallback::StaleSeed {
            seed: "seed-key".into(),
            recorded: "rec-key".into(),
        }
        .describe();
        assert!(
            stale.contains("seed-key") && stale.contains("rec-key"),
            "{stale}"
        );
    }
}
