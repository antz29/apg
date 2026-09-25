use super::*;

fn key() -> CacheKey {
    CacheKey::compute(&ScanConfigKey::default())
}

fn record(sha: &str, cache_key: CacheKey) -> ScanRecord {
    ScanRecord {
        sha: sha.to_string(),
        cache_key,
        manifest: Manifest::default(),
        content_key: None,
    }
}

/// A same-length valid-oid spelling (not a real commit: the pure
/// predicate never touches a repository).
const VALID_SHA: &str = "0123456789abcdef0123456789abcdef01234567";

/// unit tier -- pure in-memory: no filesystem, git repository or process.
/// The git-wrapper path (`full_scan_reason` discovery/ancestry over a real
/// repo) stays e2e below; these drive the extracted pure [`scan_verdict`].
mod unit {
    use super::*;

    /// fix-module-identity phase-06 task-9: the recorded-HEAD warm-cache
    /// condition and every `FullScanReason` branch are decided from the
    /// in-memory scan record + cache key + HEAD — no git discovery, no
    /// filesystem. The warm verdict is `WarmCache` (the store's facts
    /// describe this tree); every other outcome is a full scan with its
    /// reason or the ordinary incremental verdict.
    #[test]
    fn scan_verdict_decides_warm_cache_and_every_full_scan_reason_in_memory() {
        let k = key();

        // (1) NotAGitRepo — no repository at all.
        assert_eq!(
            scan_verdict(false, None, &k, Some("head"), None),
            ScanVerdict::FullScan(FullScanReason::NotAGitRepo)
        );

        // (2) NoHead — a repo with an unborn/missing HEAD.
        assert_eq!(
            scan_verdict(true, None, &k, None, None),
            ScanVerdict::FullScan(FullScanReason::NoHead)
        );

        // (3) NoRecordedScan — HEAD is present but no prior record exists.
        assert_eq!(
            scan_verdict(true, None, &k, Some("head"), None),
            ScanVerdict::FullScan(FullScanReason::NoRecordedScan)
        );

        // (4) CacheKeyDrift — the recorded key is incompatible.
        let drifted = CacheKey::compute(&ScanConfigKey {
            languages: vec!["go".into()],
            ..Default::default()
        });
        let rec = record("head", k.clone());
        assert!(matches!(
            scan_verdict(true, Some(&rec), &drifted, Some("head"), Some(true)),
            ScanVerdict::FullScan(FullScanReason::CacheKeyDrift { .. })
        ));

        // (5) WarmCache — the recorded scan is at exactly HEAD under the
        // current key (the condition `cmd_scan`'s zero-frontend path gates on).
        let warm = scan_verdict(true, Some(&rec), &k, Some("head"), Some(true));
        assert!(warm.is_warm_cache(), "{warm:?}");
        assert!(!warm.requires_full_scan());
        assert!(warm.full_scan_reason().is_none());

        // (6) DeltaUnavailable — a malformed recorded sha cannot be verified.
        let bad = record("not-a-sha", k.clone());
        assert_eq!(
            scan_verdict(true, Some(&bad), &k, Some("head"), Some(true)),
            ScanVerdict::FullScan(FullScanReason::DeltaUnavailable)
        );

        // (7) NotAncestor — a valid recorded sha that is not an ancestor of
        // HEAD (a rewrite / a GC'd commit: the wrapper passes false/None).
        let other = record(VALID_SHA, k.clone());
        assert!(matches!(
            scan_verdict(true, Some(&other), &k, Some("head"), Some(false)),
            ScanVerdict::FullScan(FullScanReason::NotAncestor { .. })
        ));
        assert!(matches!(
            scan_verdict(true, Some(&other), &k, Some("head"), None),
            ScanVerdict::FullScan(FullScanReason::NotAncestor { .. })
        ));

        // (8) Incremental — a usable ancestor under the current key.
        assert_eq!(
            scan_verdict(true, Some(&other), &k, Some("head"), Some(true)),
            ScanVerdict::Incremental
        );
    }

    /// Every `FullScanReason` renders its stable tag and human line (the
    /// scan log's `describe()`), including the warm-cache path's complement.
    #[test]
    fn full_scan_reason_tags_and_describes_every_variant() {
        let variants = [
            (FullScanReason::NotAGitRepo, "not-a-git-repo"),
            (FullScanReason::NoRecordedScan, "no-recorded-scan"),
            (FullScanReason::NoHead, "no-head"),
            (
                FullScanReason::NotAncestor {
                    recorded: "a".repeat(40),
                    head: "b".repeat(40),
                },
                "recorded-not-ancestor",
            ),
            (
                FullScanReason::CacheKeyDrift {
                    recorded: "old".into(),
                    current: "new".into(),
                },
                "cache-key-drift",
            ),
            (FullScanReason::NoPreviousExport, "no-previous-export"),
            (FullScanReason::DeltaUnavailable, "delta-unavailable"),
        ];
        for (reason, tag) in &variants {
            assert_eq!(reason.tag(), *tag);
            assert!(!reason.describe().is_empty(), "{tag}");
        }
    }
}
