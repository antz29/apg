use super::*;

/// unit tier -- pure in-memory: no filesystem, database, git or process. The
/// changelog slicer is one pure function (`render_migration_guide`) over a
/// changelog string; the version comparisons and the section extraction are
/// its private parts, tested through it.
mod unit {
    use super::*;

    /// A synthetic changelog exercising every convention: a `## vX.Y.Z` header
    /// per entry, an inline `### Migration` mention in the intro prose (never a
    /// heading, so ignored), entries with and without a Migration section, and
    /// `0.22.10` after `0.22.2` so numeric — not lexical — patch ordering shows.
    const LOG: &str = "\
# Changelog

Prose mentioning `### Migration` inline, which is not a heading.

## v0.25.0

Notes for 0.25.

### Migration

- do the 0.25 thing
- do the second 0.25 thing

## v0.24.0

No model change.

## v0.23.0

### Migration

- reverse the spine
- invent the role

## v0.22.10

### Migration

- patch-ordered task

## v0.22.2

Patch only.
";

    #[test]
    fn fresh_init_selects_nothing() {
        let guide = render_migration_guide(LOG, None, "0.25.0");
        assert!(
            guide.contains("No migration required"),
            "a fresh init has no prior version: {guide}"
        );
        assert!(!guide.contains("do the 0.25 thing"), "{guide}");
    }

    #[test]
    fn range_is_old_exclusive_and_new_inclusive() {
        let guide = render_migration_guide(LOG, Some("0.23.0"), "0.25.0");
        assert!(guide.contains("0.23.0 → 0.25.0"), "{guide}");
        assert!(guide.contains("do the 0.25 thing"), "{guide}");
        // v0.23.0 is the OLD bound: strictly greater excludes it.
        assert!(
            !guide.contains("reverse the spine"),
            "the old version's own section is excluded: {guide}"
        );
        assert!(!guide.contains("patch-ordered task"), "{guide}");
    }

    #[test]
    fn concatenates_selected_sections_in_ascending_version_order() {
        let guide = render_migration_guide(LOG, Some("0.22.2"), "0.25.0");
        let patch = guide
            .find("patch-ordered task")
            .expect("0.22.10 is in range");
        let spine = guide.find("reverse the spine").expect("0.23.0 is in range");
        let last = guide.find("do the 0.25 thing").expect("0.25.0 is in range");
        assert!(
            patch < spine && spine < last,
            "sections compose in ascending version order (0.22.10 < 0.23.0 < 0.25.0): {guide}"
        );
        assert!(guide.contains("### v0.23.0"), "{guide}");
        assert!(guide.contains("### v0.25.0"), "{guide}");
    }

    #[test]
    fn an_entry_without_a_migration_section_contributes_nothing() {
        // (0.23.0, 0.24.0] selects only v0.24.0, which carries no section.
        let guide = render_migration_guide(LOG, Some("0.23.0"), "0.24.0");
        assert!(guide.contains("No migration required"), "{guide}");
    }

    #[test]
    fn same_version_is_a_no_op() {
        let guide = render_migration_guide(LOG, Some("0.25.0"), "0.25.0");
        assert!(guide.contains("No migration required"), "{guide}");
    }

    #[test]
    fn a_malformed_old_version_lists_every_earlier_migration() {
        // Unparseable old → treated as the floor, so the guide errs toward
        // listing the migration rather than silently skipping it.
        let guide = render_migration_guide(LOG, Some("banana"), "0.25.0");
        for needle in [
            "patch-ordered task",
            "reverse the spine",
            "do the 0.25 thing",
        ] {
            assert!(guide.contains(needle), "{needle}: {guide}");
        }
    }

    #[test]
    fn the_shipped_changelog_slices_the_0_22_to_0_23_migration() {
        let guide = render_migration_guide(CHANGELOG, Some("0.22.2"), "0.23.0");
        for needle in [
            "realises",
            "depends-on",
            "drives",
            "represents",
            "attaches-to",
            "domain.actor",
            "solution.role",
            "is-role",
        ] {
            assert!(
                guide.contains(needle),
                "the shipped 0.23 migration: {needle}"
            );
        }
    }

    #[test]
    fn the_shipped_changelog_is_a_no_op_for_the_current_binary() {
        // A fresh init (no prior version) never needs a migration, whatever
        // the binary version is.
        let guide = render_migration_guide(CHANGELOG, None, env!("CARGO_PKG_VERSION"));
        assert!(guide.contains("No migration required"), "{guide}");
    }
}
