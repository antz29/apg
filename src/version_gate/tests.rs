use super::*;

/// A fixed binary version for the pure rule tests (they never depend on
/// the crate's own version, so they survive release bumps).
const BIN: &str = "0.10.4";

// ------------------------------------------------------------------
// task-2/task-5 rules: same major.minor proceeds (patch diff fine);
// missing blocks; major/minor mismatch in EITHER direction blocks.
// ------------------------------------------------------------------

/// unit tier -- pure in-memory: no filesystem, database, git or process.
/// The rule/block-text tests take version VALUES (and `Path`s), not files.
mod unit {
    use super::*;

    #[test]
    fn same_major_minor_proceeds_any_patch() {
        for v in ["0.10.0", "0.10.3", "0.10.4", "0.10.99"] {
            assert_eq!(check_version(BIN, Some(v)), Ok(()), "layout {v}");
        }
        // Two-part versions (no patch component) parse and compare too.
        assert_eq!(check_version(BIN, Some("0.10")), Ok(()));
    }

    #[test]
    fn missing_version_blocks() {
        assert_eq!(check_version(BIN, None), Err(VersionBlock::Unversioned));
    }

    #[test]
    fn malformed_version_blocks() {
        for v in ["", "banana", "0", "v0.10.4", "0.10-beta", "0..4"] {
            assert_eq!(
                check_version(BIN, Some(v)),
                Err(VersionBlock::Malformed {
                    raw: v.to_string()
                }),
                "layout `{v}` must be malformed"
            );
        }
    }

    #[test]
    fn older_major_or_minor_blocks() {
        for v in ["0.9.0", "0.9.12", "0.0.1", "0.0.0"] {
            assert!(
                matches!(
                    check_version(BIN, Some(v)),
                    Err(VersionBlock::Mismatch { .. })
                ),
                "layout {v} must block (older)"
            );
        }
    }

    #[test]
    fn newer_major_or_minor_blocks() {
        for v in ["0.11.0", "0.11.4", "1.0.0", "2.3.4"] {
            assert!(
                matches!(
                    check_version(BIN, Some(v)),
                    Err(VersionBlock::Mismatch { .. })
                ),
                "layout {v} must block (newer)"
            );
        }
    }

    #[test]
    fn mismatch_carries_the_layout_pair_for_direction() {
        match check_version(BIN, Some("0.9.7")) {
            Err(VersionBlock::Mismatch {
                layout_version,
                layout_major,
                layout_minor,
            }) => {
                assert_eq!(layout_version, "0.9.7");
                assert_eq!((layout_major, layout_minor), (0, 9));
            }
            other => panic!("expected mismatch, got {other:?}"),
        }
    }

    // ------------------------------------------------------------------
    // Block text (R10): upgrade guidance in both directions + doc pointer
    // ------------------------------------------------------------------

    #[test]
    fn block_messages_carry_direction_fix_and_doc_pointer() {
        let path = Path::new("/repo/apg/config.json");
        // Older layout: upgrade the layout via init.
        let older = block_message(
            &VersionBlock::Mismatch {
                layout_version: "0.9.7".into(),
                layout_major: 0,
                layout_minor: 9,
            },
            BIN,
            path,
            "re-run `apg scan`",
        );
        for needle in [
            "0.9.7",
            BIN,
            "0.9",
            "0.10",
            "apg init",
            "re-run `apg scan`",
            UPGRADE_DOC,
        ] {
            assert!(older.contains(needle), "older block text: {older}");
        }
        // Newer layout: upgrade the binary, then init.
        let newer = block_message(
            &VersionBlock::Mismatch {
                layout_version: "1.0.0".into(),
                layout_major: 1,
                layout_minor: 0,
            },
            BIN,
            path,
            "re-run `apg project start foo`",
        );
        for needle in [
            "1.0.0",
            "NEWER apg",
            "upgrade apg",
            "apg init",
            "re-run `apg project start foo`",
        ] {
            assert!(newer.contains(needle), "newer block text: {newer}");
        }
        // Unversioned: init guidance.
        let unversioned =
            block_message(&VersionBlock::Unversioned, BIN, path, "re-run `apg scan`");
        for needle in [
            "no layout version",
            "apg init",
            "re-run `apg scan`",
            UPGRADE_DOC,
        ] {
            assert!(
                unversioned.contains(needle),
                "unversioned block text: {unversioned}"
            );
        }
    }
}
