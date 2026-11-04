//! Unit tests for the dialect table. The table is a single unit, so the tests live in
//! `tests::unit` alone (no `int` tier: nothing here wires two units together).

use crate::dialect::{Dialect, Feature, FeatureLevel, Limits, SourceFormat, OO_WORDS};

const ALL_DIALECTS: [Dialect; 4] = [
    Dialect::Cobol85,
    Dialect::Cobol2002,
    Dialect::Cobol2014,
    Dialect::Ibm,
];

mod unit {
    use super::*;

    #[test]
    fn default_dialect_is_cobol85() {
        assert_eq!(Dialect::DEFAULT, Dialect::Cobol85);
    }

    #[test]
    fn from_name_accepts_exactly_the_four_dialects() {
        assert_eq!(Dialect::from_name("cobol85"), Ok(Dialect::Cobol85));
        assert_eq!(Dialect::from_name("cobol2002"), Ok(Dialect::Cobol2002));
        assert_eq!(Dialect::from_name("cobol2014"), Ok(Dialect::Cobol2014));
        assert_eq!(Dialect::from_name("ibm"), Ok(Dialect::Ibm));
    }

    #[test]
    fn from_name_refuses_cobol2023_and_unknown_names() {
        let placeholder = Dialect::from_name("cobol2023").unwrap_err();
        assert!(placeholder.contains("cobol2023"), "{placeholder}");
        let unknown = Dialect::from_name("cobol1974").unwrap_err();
        assert!(unknown.contains("unknown dialect `cobol1974`"), "{unknown}");
    }

    #[test]
    fn oo_recognition_is_off_only_under_cobol85() {
        assert!(!Dialect::Cobol85.oo_enabled());
        assert!(Dialect::Cobol2002.oo_enabled());
        assert!(Dialect::Cobol2014.oo_enabled());
        assert!(Dialect::Ibm.oo_enabled());
    }

    #[test]
    fn class_is_the_only_reserved_oo_word_under_cobol85() {
        for word in OO_WORDS {
            assert_eq!(
                Dialect::Cobol85.is_reserved(word),
                word == "CLASS",
                "{word}"
            );
        }
    }

    #[test]
    fn no_oo_word_is_reserved_under_any_dialect_but_class_under_cobol85() {
        for dialect in ALL_DIALECTS {
            for word in OO_WORDS {
                if dialect == Dialect::Cobol85 && word == "CLASS" {
                    continue;
                }
                assert!(!dialect.is_reserved(word), "{dialect:?} reserves {word}");
            }
        }
    }

    #[test]
    fn non_oo_reserved_words_per_dialect() {
        assert!(Dialect::Cobol85.is_reserved("ALTER"));
        assert!(!Dialect::Cobol85.is_reserved("ENTRY"));
        assert!(Dialect::Ibm.is_reserved("ALTER"));
        assert!(Dialect::Ibm.is_reserved("ENTRY"));
        for dialect in [Dialect::Cobol2002, Dialect::Cobol2014] {
            for word in ["ALTER", "ENTRY", "CLASS"] {
                assert!(!dialect.is_reserved(word), "{dialect:?} reserves {word}");
            }
        }
    }

    #[test]
    fn reserved_word_match_is_case_insensitive() {
        assert!(Dialect::Cobol85.is_reserved("class"));
        assert!(Dialect::Cobol85.is_reserved("Alter"));
        assert!(Dialect::Ibm.is_reserved("entry"));
    }

    #[test]
    fn oo_word_match_is_case_insensitive() {
        assert!(crate::dialect::is_oo_word("object"));
        assert!(crate::dialect::is_oo_word("INVOKE"));
        assert!(!crate::dialect::is_oo_word("ALTER"));
    }

    #[test]
    fn length_limits_per_dialect() {
        let limits = |word, literal, pic| Limits { word, literal, pic };
        assert_eq!(Dialect::Cobol85.limits(), limits(30, 160, 30));
        assert_eq!(Dialect::Cobol2002.limits(), limits(31, 160, 50));
        assert_eq!(Dialect::Cobol2014.limits(), limits(31, 8191, 63));
        assert_eq!(Dialect::Ibm.limits(), limits(30, 160, 50));
    }

    #[test]
    fn error_and_unconformable_skip_the_construct_with_a_diagnostic() {
        for level in [FeatureLevel::Error, FeatureLevel::Unconformable] {
            assert!(level.emits_diagnostic(), "{level:?}");
            assert!(level.skips_construct(), "{level:?}");
        }
    }

    #[test]
    fn warning_archaic_and_obsolete_diagnose_and_still_scan() {
        for level in [
            FeatureLevel::Warning,
            FeatureLevel::Archaic,
            FeatureLevel::Obsolete,
        ] {
            assert!(level.emits_diagnostic(), "{level:?}");
            assert!(!level.skips_construct(), "{level:?}");
        }
    }

    #[test]
    fn ok_ignore_and_skip_are_silent_and_scan() {
        for level in [FeatureLevel::Ok, FeatureLevel::Ignore, FeatureLevel::Skip] {
            assert!(!level.emits_diagnostic(), "{level:?}");
            assert!(!level.skips_construct(), "{level:?}");
        }
    }

    #[test]
    fn starting_feature_levels_apply_under_every_dialect() {
        for dialect in ALL_DIALECTS {
            assert_eq!(
                dialect.feature_level(Feature::Alter),
                FeatureLevel::Obsolete
            );
            assert_eq!(
                dialect.feature_level(Feature::GoToWithoutName),
                FeatureLevel::Obsolete
            );
            assert_eq!(dialect.feature_level(Feature::Entry), FeatureLevel::Ok);
            assert_eq!(
                dialect.feature_level(Feature::SectionSegments),
                FeatureLevel::Ignore
            );
        }
    }

    #[test]
    fn default_format_per_dialect() {
        assert_eq!(Dialect::Cobol85.default_format(), SourceFormat::Cobol85);
        assert_eq!(Dialect::Ibm.default_format(), SourceFormat::Cobol85);
        assert_eq!(Dialect::Cobol2002.default_format(), SourceFormat::Fixed);
        assert_eq!(Dialect::Cobol2014.default_format(), SourceFormat::Fixed);
    }
}
