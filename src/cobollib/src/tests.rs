//! Unit tests for the dialect table. The table is a single unit, so the tests live in
//! `tests::unit` alone (no `int` tier: nothing here wires two units together).

use crate::dialect::{Dialect, Feature, FeatureLevel, Limits, SourceFormat, OO_WORDS};
use crate::lexer::{lex, Token, TokenKind};

const ALL_DIALECTS: [Dialect; 4] = [
    Dialect::Cobol85,
    Dialect::Cobol2002,
    Dialect::Cobol2014,
    Dialect::Ibm,
];

/// A line with `text` starting at column `col` (1-based).
fn at(col: usize, text: &str) -> String {
    format!("{}{}", " ".repeat(col - 1), text)
}

/// Lexes `text` under `dialect` and returns the tokens and the diagnostics.
fn lex_all(
    text: &str,
    layout: crate::format::Layout,
    dialect: Dialect,
) -> (Vec<Token>, Vec<crate::diag::Diagnostic>) {
    let (mut cursor, diags) = lex("p.cob", text, layout, dialect);
    let mut tokens = Vec::new();
    while let Some(token) = cursor.bump() {
        tokens.push(token);
    }
    (tokens, diags)
}

/// The fixed (`cobol85`) column layout.
fn fixed() -> crate::format::Layout {
    crate::format::Layout::Source(SourceFormat::Cobol85)
}

/// Each token as its kind and text.
fn shape(tokens: &[Token]) -> Vec<(TokenKind, &str)> {
    tokens.iter().map(|t| (t.kind, t.text.as_str())).collect()
}

mod unit {
    use super::*;
    use crate::format::Layout;

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
        assert_eq!(
            Dialect::from_name("cobol2023").unwrap_err(),
            "dialect cobol2023 is a placeholder with no features yet; use cobol85, cobol2002, cobol2014 or ibm"
        );
        assert_eq!(
            Dialect::from_name("cobol1974").unwrap_err(),
            "unknown dialect `cobol1974`; expected cobol85, cobol2002, cobol2014 or ibm"
        );
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

    #[test]
    fn directive_free_sets_the_free_layout() {
        let text = [at(8, ">>source format free"), at(8, "DISPLAY X.")].join("\n");
        let (layout, diags) = crate::format::check("p.cob", &text, Dialect::Cobol85);
        assert_eq!(layout, Layout::Free);
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn directive_fixed_turns_area_a_checks_off() {
        let text = [at(8, ">>SOURCE IS FIXED"), at(12, "PROCEDURE DIVISION.")].join("\n");
        let (layout, diags) = crate::format::check("p.cob", &text, Dialect::Cobol85);
        assert_eq!(layout, Layout::Source(SourceFormat::Fixed));
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn directive_cobol85_turns_area_a_checks_on() {
        let text = [at(8, ">>SOURCE COBOL85"), at(12, "PROCEDURE DIVISION.")].join("\n");
        let (layout, diags) = crate::format::check("p.cob", &text, Dialect::Cobol2002);
        assert_eq!(layout, Layout::Source(SourceFormat::Cobol85));
        assert_eq!(diags.len(), 1, "{diags:?}");
    }

    #[test]
    fn unknown_directive_format_is_diagnosed_and_reads_the_dialect_default() {
        let text = at(8, ">>SOURCE FORMAT VARIABLE");
        let (layout, diags) = crate::format::check("p.cob", &text, Dialect::Cobol85);
        assert_eq!(layout, Layout::Source(SourceFormat::Cobol85));
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!((diags[0].line, diags[0].col), (1, 8));
        assert!(diags[0].message.contains("VARIABLE"), "{diags:?}");
    }

    #[test]
    fn clean_fixed_text_has_no_warning_and_no_area_a_checks() {
        let text = [
            at(8, "IDENTIFICATION DIVISION."),
            at(12, "PROCEDURE DIVISION."),
            at(12, "DISPLAY X."),
        ]
        .join("\n");
        let (layout, diags) = crate::format::check("p.cob", &text, Dialect::Cobol2002);
        assert_eq!(layout, Layout::Source(SourceFormat::Fixed));
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn free_looking_text_warns_naming_the_file_and_reads_fixed() {
        let text = at(1, "IDENTIFICATION DIVISION.");
        let (layout, diags) = crate::format::check("prog.cob", &text, Dialect::Cobol2002);
        assert_eq!(layout, Layout::Source(SourceFormat::Fixed));
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].file, "prog.cob");
        assert_eq!((diags[0].line, diags[0].col), (1, 1));
        assert!(diags[0].message.contains("free-format"), "{diags:?}");
        assert!(diags[0].message.contains("fixed format"), "{diags:?}");
    }

    #[test]
    fn text_past_column_80_warns_but_identification_area_does_not() {
        let past = format!("{}X", " ".repeat(80));
        let (_, diags) = crate::format::check("p.cob", &past, Dialect::Cobol2002);
        assert_eq!(diags.len(), 1, "{diags:?}");
        let ident = at(73, "PROG0001");
        let (_, diags) = crate::format::check("p.cob", &ident, Dialect::Cobol2002);
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn indicators_accepted_and_slash_is_not() {
        let ok = [at(7, "*"), at(7, "D"), at(7, "-")].join("\n");
        let (_, diags) = crate::format::check("p.cob", &ok, Dialect::Cobol2002);
        assert!(diags.is_empty(), "{diags:?}");
        let slash = at(7, "/ comment");
        let (_, diags) = crate::format::check("p.cob", &slash, Dialect::Cobol2002);
        assert_eq!(diags.len(), 1, "{diags:?}");
    }

    #[test]
    fn division_outside_area_a_is_a_diagnostic() {
        let text = [
            at(8, "IDENTIFICATION DIVISION."),
            at(12, "PROCEDURE DIVISION."),
        ]
        .join("\n");
        let (_, diags) = crate::format::check("p.cob", &text, Dialect::Cobol85);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!((diags[0].line, diags[0].col), (2, 12));
    }

    #[test]
    fn paragraph_outside_area_a_is_a_diagnostic() {
        let text = [at(8, "PROCEDURE DIVISION."), at(12, "MAIN-PARA.")].join("\n");
        let (_, diags) = crate::format::check("p.cob", &text, Dialect::Cobol85);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].line, 2);
    }

    #[test]
    fn level_01_and_77_outside_area_a_are_diagnostics_and_level_05_is_not_checked() {
        let text = [
            at(8, "WORKING-STORAGE SECTION."),
            at(12, "01 REC."),
            at(12, "77 W PIC X."),
            at(12, "05 F PIC X."),
        ]
        .join("\n");
        let (_, diags) = crate::format::check("p.cob", &text, Dialect::Cobol85);
        let lines: Vec<usize> = diags.iter().map(|d| d.line).collect();
        assert_eq!(lines, [2, 3], "{diags:?}");
    }

    #[test]
    fn statement_in_area_a_is_a_diagnostic() {
        let text = [at(8, "PROCEDURE DIVISION."), at(8, "DISPLAY X.")].join("\n");
        let (_, diags) = crate::format::check("p.cob", &text, Dialect::Cobol85);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!((diags[0].line, diags[0].col), (2, 8));
    }

    #[test]
    fn separator_period_in_area_a_is_a_diagnostic() {
        let text = [at(12, "DISPLAY X"), at(8, ".")].join("\n");
        let (_, diags) = crate::format::check("p.cob", &text, Dialect::Cobol85);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].line, 2);
    }

    #[test]
    fn valid_cobol85_program_has_no_area_a_diagnostics() {
        let text = [
            at(8, "IDENTIFICATION DIVISION."),
            at(8, "PROGRAM-ID. PROG."),
            at(8, "PROCEDURE DIVISION."),
            at(8, "MAIN-PARA."),
            at(12, "DISPLAY X."),
            at(12, "STOP RUN."),
            at(8, "01 REC."),
            at(12, "05 F PIC X."),
            at(8, "77 W PIC X."),
            at(12, "."),
        ]
        .join("\n");
        let (_, diags) = crate::format::check("p.cob", &text, Dialect::Cobol85);
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn continuation_and_comment_lines_are_not_area_a_checked() {
        let text = [
            at(8, "PROCEDURE DIVISION."),
            at(12, "DISPLAY \"A"),
            format!("{}\"B\".", at(7, "-")),
            at(7, "* MOVE X TO Y."),
        ]
        .join("\n");
        let (_, diags) = crate::format::check("p.cob", &text, Dialect::Cobol85);
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn fixed_layout_ignores_sequence_area_and_columns_73_to_80() {
        let text = format!("000100 {:<65}{}", "DISPLAY X.", "PROG0001");
        let (tokens, _) = lex_all(&text, fixed(), Dialect::Cobol85);
        assert_eq!(
            shape(&tokens),
            [
                (TokenKind::Word, "DISPLAY"),
                (TokenKind::Word, "X"),
                (TokenKind::Period, ".")
            ]
        );
    }

    #[test]
    fn comment_and_debugging_lines_are_skipped() {
        let text = [
            at(7, "* DISPLAY A."),
            at(7, "D DISPLAY B."),
            at(8, "DISPLAY C."),
        ]
        .join("\n");
        let (tokens, _) = lex_all(&text, fixed(), Dialect::Cobol85);
        assert_eq!(
            shape(&tokens),
            [
                (TokenKind::Word, "DISPLAY"),
                (TokenKind::Word, "C"),
                (TokenKind::Period, ".")
            ]
        );
    }

    #[test]
    fn directive_lines_are_not_tokens() {
        let text = [at(8, ">>SOURCE FORMAT FREE"), at(8, "DISPLAY X.")].join("\n");
        let (tokens, _) = lex_all(&text, fixed(), Dialect::Cobol85);
        assert_eq!(tokens[0].text, "DISPLAY");
    }

    #[test]
    fn continued_word_joins_across_the_continuation_line() {
        let text = [at(8, "PROGRAM-"), format!("{}ID.", at(7, "-"))].join("\n");
        let (tokens, _) = lex_all(&text, fixed(), Dialect::Cobol85);
        assert_eq!(
            shape(&tokens),
            [(TokenKind::Word, "PROGRAM-ID"), (TokenKind::Period, ".")]
        );
    }

    #[test]
    fn continued_literal_resumes_after_the_quote_in_area_b() {
        let text = [at(8, "DISPLAY \"AB"), format!("{}\"CD\".", at(7, "-"))].join("\n");
        let (tokens, diags) = lex_all(&text, fixed(), Dialect::Cobol85);
        assert_eq!(
            shape(&tokens),
            [
                (TokenKind::Word, "DISPLAY"),
                (TokenKind::Alnum, "ABCD"),
                (TokenKind::Period, ".")
            ]
        );
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn doubled_quote_is_one_quote_in_both_quote_styles() {
        let (tokens, _) = lex_all(&at(8, "\"A\"\"B\" 'IT''S'"), fixed(), Dialect::Cobol85);
        assert_eq!(
            shape(&tokens),
            [(TokenKind::Alnum, "A\"B"), (TokenKind::Alnum, "IT'S")]
        );
    }

    #[test]
    fn national_literal_is_its_own_kind() {
        let (tokens, _) = lex_all(&at(8, "N\"XY\""), fixed(), Dialect::Cobol85);
        assert_eq!(shape(&tokens), [(TokenKind::National, "XY")]);
    }

    #[test]
    fn numeric_literals_take_sign_and_decimal_point() {
        let (tokens, _) = lex_all(
            &at(8, "MOVE +1.5 -3 .5 10 TO X."),
            fixed(),
            Dialect::Cobol85,
        );
        assert_eq!(
            shape(&tokens),
            [
                (TokenKind::Word, "MOVE"),
                (TokenKind::Numeric, "+1.5"),
                (TokenKind::Numeric, "-3"),
                (TokenKind::Numeric, ".5"),
                (TokenKind::Numeric, "10"),
                (TokenKind::Word, "TO"),
                (TokenKind::Word, "X"),
                (TokenKind::Period, ".")
            ]
        );
    }

    #[test]
    fn period_after_a_number_is_a_separator() {
        let (tokens, _) = lex_all(&at(8, "MOVE 10."), fixed(), Dialect::Cobol85);
        assert_eq!(
            shape(&tokens),
            [
                (TokenKind::Word, "MOVE"),
                (TokenKind::Numeric, "10"),
                (TokenKind::Period, ".")
            ]
        );
    }

    #[test]
    fn pseudo_text_delimiters_and_separators() {
        let (tokens, _) = lex_all(&at(8, "==A==, B; (C)"), fixed(), Dialect::Cobol85);
        assert_eq!(
            shape(&tokens),
            [
                (TokenKind::PseudoDelim, "=="),
                (TokenKind::Word, "A"),
                (TokenKind::PseudoDelim, "=="),
                (TokenKind::Comma, ","),
                (TokenKind::Word, "B"),
                (TokenKind::Semicolon, ";"),
                (TokenKind::LParen, "("),
                (TokenKind::Word, "C"),
                (TokenKind::RParen, ")")
            ]
        );
    }

    #[test]
    fn reserved_words_follow_the_dialect() {
        let (tokens, _) = lex_all(&at(8, "class OBJECT"), fixed(), Dialect::Cobol85);
        assert_eq!(
            shape(&tokens),
            [(TokenKind::Reserved, "class"), (TokenKind::Word, "OBJECT")]
        );
        let (tokens, _) = lex_all(&at(8, "CLASS"), fixed(), Dialect::Cobol2002);
        assert_eq!(shape(&tokens), [(TokenKind::Word, "CLASS")]);
    }

    #[test]
    fn every_oo_word_but_class_is_an_ordinary_word_under_cobol85() {
        for word in OO_WORDS {
            let (tokens, _) = lex_all(&at(8, word), fixed(), Dialect::Cobol85);
            let kind = if word == "CLASS" {
                TokenKind::Reserved
            } else {
                TokenKind::Word
            };
            assert_eq!(shape(&tokens), [(kind, word)], "{word}");
        }
    }

    #[test]
    fn word_over_the_dialect_limit_is_a_diagnostic_and_still_a_token() {
        let (_, diags) = lex_all(&at(8, &"A".repeat(30)), fixed(), Dialect::Cobol85);
        assert!(diags.is_empty(), "{diags:?}");
        let (tokens, diags) = lex_all(&at(8, &"A".repeat(31)), fixed(), Dialect::Cobol85);
        assert_eq!(tokens.len(), 1);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!((diags[0].line, diags[0].col), (1, 8));
        let (_, diags) = lex_all(&at(8, &"A".repeat(31)), fixed(), Dialect::Cobol2002);
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn literal_over_the_dialect_limit_is_a_diagnostic_and_still_a_token() {
        let text = format!("\"{}\"", "X".repeat(160));
        let (_, diags) = lex_all(&text, Layout::Free, Dialect::Cobol85);
        assert!(diags.is_empty(), "{diags:?}");
        let text = format!("\"{}\"", "X".repeat(161));
        let (tokens, diags) = lex_all(&text, Layout::Free, Dialect::Cobol85);
        assert_eq!(tokens.len(), 1);
        assert_eq!(diags.len(), 1, "{diags:?}");
    }

    #[test]
    fn unterminated_literal_is_a_diagnostic() {
        let (tokens, diags) = lex_all("DISPLAY \"ABC", Layout::Free, Dialect::Cobol85);
        assert_eq!(
            shape(&tokens),
            [(TokenKind::Word, "DISPLAY"), (TokenKind::Alnum, "ABC")]
        );
        assert_eq!(diags.len(), 1, "{diags:?}");
    }

    #[test]
    fn free_format_reads_from_column_one_and_comments_after_star_greater() {
        let (tokens, _) = lex_all("DISPLAY \"*>\" *> note", Layout::Free, Dialect::Cobol85);
        assert_eq!(
            shape(&tokens),
            [(TokenKind::Word, "DISPLAY"), (TokenKind::Alnum, "*>")]
        );
        assert_eq!(tokens[0].col, 1);
    }

    #[test]
    fn token_positions_refer_to_the_original_text() {
        let text = [at(8, "DISPLAY X."), at(12, "STOP RUN.")].join("\n");
        let (tokens, _) = lex_all(&text, fixed(), Dialect::Cobol85);
        let stop = tokens.iter().find(|t| t.text == "STOP").unwrap();
        assert_eq!(
            (stop.offset, stop.line, stop.col),
            (text.find("STOP").unwrap(), 2, 12)
        );
    }

    #[test]
    fn cursor_peeks_and_bumps_in_order() {
        let (mut cursor, _) = lex("p.cob", &at(8, "A B"), fixed(), Dialect::Cobol85);
        assert_eq!(cursor.peek().map(|t| t.text.as_str()), Some("A"));
        assert_eq!(cursor.peek_nth(1).map(|t| t.text.as_str()), Some("B"));
        assert_eq!(cursor.bump().map(|t| t.text), Some("A".to_string()));
        assert!(!cursor.at_end());
        cursor.bump();
        assert!(cursor.bump().is_none());
        assert!(cursor.at_end());
    }

    /// Lexes `text` under cobol85 and applies its REPLACE statements.
    fn replaced(text: &str) -> (Vec<Token>, Vec<crate::diag::Diagnostic>) {
        let (tokens, _) = lex_all(text, fixed(), Dialect::Cobol85);
        crate::replace::replace("p.cob", tokens)
    }

    #[test]
    fn replace_swaps_a_matching_pseudo_text() {
        let (tokens, diags) = replaced(&at(8, "REPLACE ==A== BY ==B==. DISPLAY A."));
        assert_eq!(
            shape(&tokens),
            [
                (TokenKind::Word, "DISPLAY"),
                (TokenKind::Word, "B"),
                (TokenKind::Period, ".")
            ]
        );
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn replace_matches_words_case_insensitively() {
        let (tokens, _) = replaced(&at(8, "REPLACE ==display== BY ==SHOW==. DISPLAY X."));
        assert_eq!(
            shape(&tokens),
            [
                (TokenKind::Word, "SHOW"),
                (TokenKind::Word, "X"),
                (TokenKind::Period, ".")
            ]
        );
    }

    #[test]
    fn replace_ignores_separators_in_the_source_and_the_pattern() {
        let (tokens, _) = replaced(&at(8, "REPLACE ==A B== BY ==C==. MOVE A, B TO X."));
        assert_eq!(
            shape(&tokens),
            [
                (TokenKind::Word, "MOVE"),
                (TokenKind::Word, "C"),
                (TokenKind::Word, "TO"),
                (TokenKind::Word, "X"),
                (TokenKind::Period, ".")
            ]
        );
    }

    #[test]
    fn replaced_token_keeps_the_position_of_the_token_it_replaces() {
        let text = [at(8, "REPLACE ==A== BY ==B==."), at(8, "DISPLAY A.")].join("\n");
        let (tokens, _) = replaced(&text);
        let b = tokens.iter().find(|t| t.text == "B").unwrap();
        assert_eq!(
            (b.offset, b.line, b.col),
            (text.rfind("A.").unwrap(), 2, 16)
        );
    }

    #[test]
    fn replace_applies_only_to_the_text_after_its_statement() {
        let (tokens, _) = replaced(&at(8, "DISPLAY A. REPLACE ==A== BY ==B==. DISPLAY A."));
        assert_eq!(
            shape(&tokens),
            [
                (TokenKind::Word, "DISPLAY"),
                (TokenKind::Word, "A"),
                (TokenKind::Period, "."),
                (TokenKind::Word, "DISPLAY"),
                (TokenKind::Word, "B"),
                (TokenKind::Period, ".")
            ]
        );
    }

    #[test]
    fn replace_off_stops_replacing() {
        let (tokens, _) = replaced(&at(
            8,
            "REPLACE ==A== BY ==B==. DISPLAY A. REPLACE OFF. DISPLAY A.",
        ));
        assert_eq!(
            shape(&tokens),
            [
                (TokenKind::Word, "DISPLAY"),
                (TokenKind::Word, "B"),
                (TokenKind::Period, "."),
                (TokenKind::Word, "DISPLAY"),
                (TokenKind::Word, "A"),
                (TokenKind::Period, ".")
            ]
        );
    }

    #[test]
    fn a_later_replace_statement_replaces_the_earlier_pairs() {
        let (tokens, _) = replaced(&at(
            8,
            "REPLACE ==A== BY ==B==. REPLACE ==C== BY ==D==. DISPLAY A C.",
        ));
        assert_eq!(
            shape(&tokens),
            [
                (TokenKind::Word, "DISPLAY"),
                (TokenKind::Word, "A"),
                (TokenKind::Word, "D"),
                (TokenKind::Period, ".")
            ]
        );
    }

    #[test]
    fn one_statement_may_hold_several_pairs_and_an_empty_replacement() {
        let (tokens, _) = replaced(&at(
            8,
            "REPLACE ==A== BY ==X== ==C== BY ====. DISPLAY A C B.",
        ));
        assert_eq!(
            shape(&tokens),
            [
                (TokenKind::Word, "DISPLAY"),
                (TokenKind::Word, "X"),
                (TokenKind::Word, "B"),
                (TokenKind::Period, ".")
            ]
        );
    }

    #[test]
    fn empty_pattern_matches_nothing() {
        let (tokens, _) = replaced(&at(8, "REPLACE ==,== BY ==B==. DISPLAY A."));
        assert_eq!(
            shape(&tokens),
            [
                (TokenKind::Word, "DISPLAY"),
                (TokenKind::Word, "A"),
                (TokenKind::Period, ".")
            ]
        );
    }

    #[test]
    fn malformed_replace_is_a_diagnostic_and_skips_its_sentence() {
        let (tokens, diags) = replaced(&at(8, "REPLACE ==A== DISPLAY A. DISPLAY B."));
        assert_eq!(
            shape(&tokens),
            [
                (TokenKind::Word, "DISPLAY"),
                (TokenKind::Word, "B"),
                (TokenKind::Period, ".")
            ]
        );
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!((diags[0].line, diags[0].col), (1, 8));
    }

    #[test]
    fn source_map_names_each_file_and_keeps_token_positions() {
        let mut map = crate::source_map::SourceMap::default();
        let main = map.add_file("prog.cob");
        let copy = map.add_file("copy.cpy");
        assert_eq!(map.name(copy), "copy.cpy");
        let (tokens, _) = lex_all(&at(8, "DISPLAY X."), fixed(), Dialect::Cobol85);
        let mapped = map.map(copy, tokens);
        assert_eq!(mapped.len(), 3);
        assert_eq!(
            mapped[1].origin,
            crate::source_map::Origin {
                file: copy,
                line: 1,
                col: 16
            }
        );
        assert_ne!(mapped[0].origin.file, main);
    }

    #[test]
    fn source_map_locates_a_diagnostic_in_the_copybook_file() {
        let mut map = crate::source_map::SourceMap::default();
        map.add_file("prog.cob");
        let copy = map.add_file("copy.cpy");
        let at_copy = crate::source_map::Origin {
            file: copy,
            line: 3,
            col: 9,
        };
        let diag = map.diagnostic(at_copy, "unknown word FOO");
        assert_eq!(
            diag,
            crate::diag::Diagnostic {
                file: "copy.cpy".to_string(),
                line: 3,
                col: 9,
                message: "unknown word FOO".to_string(),
            }
        );
    }

    /// Each expanded token as (text, file, line, col).
    type Expanded = Vec<(String, String, usize, usize)>;

    /// Expands `main` (named `name`) against the in-memory `files`; the tokens and the diagnostics.
    fn run(
        name: &str,
        main: &str,
        files: &[(&str, &str)],
        search: &crate::preprocess::Search,
    ) -> (Expanded, Vec<crate::diag::Diagnostic>) {
        let store: std::collections::HashMap<std::path::PathBuf, String> = files
            .iter()
            .map(|(p, t)| (std::path::PathBuf::from(p), t.to_string()))
            .collect();
        let lookup = |path: &std::path::Path| store.get(path).cloned();
        let mut map = crate::source_map::SourceMap::default();
        let (mapped, diags) =
            crate::preprocess::preprocess(&mut map, name, main, Dialect::Cobol85, search, &lookup);
        let tokens = mapped
            .iter()
            .map(|m| {
                (
                    m.token.text.clone(),
                    map.name(m.origin.file).to_string(),
                    m.origin.line,
                    m.origin.col,
                )
            })
            .collect();
        (tokens, diags)
    }

    /// Lines of fixed-format text, each statement at column 12.
    fn src(lines: &[&str]) -> String {
        lines
            .iter()
            .map(|l| at(12, l))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn search() -> crate::preprocess::Search {
        crate::preprocess::Search {
            scan_root: "/root".into(),
            copy_paths: Vec::new(),
        }
    }

    fn texts(tokens: &[(String, String, usize, usize)]) -> Vec<&str> {
        tokens.iter().map(|t| t.0.as_str()).collect()
    }

    /// Copybooks `L1`..`L{len}` under /root: each copies the next, the last displays `last`.
    fn chain(len: usize, last: &str) -> Vec<(String, String)> {
        (1..=len)
            .map(|i| {
                let text = if i < len {
                    src(&[&format!("COPY L{}.", i + 1)])
                } else {
                    src(&[&format!("DISPLAY {last}.")])
                };
                (format!("/root/L{i}.cpy"), text)
            })
            .collect()
    }

    #[test]
    fn copy_splices_the_copybook_text_in_place() {
        let (tokens, diags) = run(
            "/root/prog.cob",
            &src(&["COPY A.", "STOP RUN."]),
            &[("/root/A.cpy", &src(&["DISPLAY X."]))],
            &search(),
        );
        assert_eq!(texts(&tokens), ["DISPLAY", "X", ".", "STOP", "RUN", "."]);
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn copied_and_main_tokens_keep_their_own_file_and_position() {
        let (tokens, _) = run(
            "/root/prog.cob",
            &src(&["COPY A.", "STOP RUN."]),
            &[("/root/A.cpy", &src(&["DISPLAY X."]))],
            &search(),
        );
        let x = &tokens[1];
        assert_eq!((x.1.as_str(), x.2, x.3), ("/root/A.cpy", 1, 20));
        let stop = &tokens[3];
        assert_eq!((stop.1.as_str(), stop.2, stop.3), ("/root/prog.cob", 2, 12));
    }

    #[test]
    fn copy_replacing_swaps_pseudo_text() {
        let (tokens, diags) = run(
            "/root/prog.cob",
            &src(&["COPY A REPLACING ==X== BY ==Y==."]),
            &[("/root/A.cpy", &src(&["DISPLAY X."]))],
            &search(),
        );
        assert_eq!(texts(&tokens), ["DISPLAY", "Y", "."]);
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn copy_replacing_takes_bare_word_pairs() {
        let (tokens, _) = run(
            "/root/prog.cob",
            &src(&["COPY A REPLACING X BY Y."]),
            &[("/root/A.cpy", &src(&["DISPLAY X."]))],
            &search(),
        );
        assert_eq!(texts(&tokens), ["DISPLAY", "Y", "."]);
    }

    #[test]
    fn main_replace_applies_to_copied_text() {
        let (tokens, _) = run(
            "/root/prog.cob",
            &src(&["REPLACE ==X== BY ==Z==.", "COPY A."]),
            &[("/root/A.cpy", &src(&["DISPLAY X."]))],
            &search(),
        );
        assert_eq!(texts(&tokens), ["DISPLAY", "Z", "."]);
    }

    #[test]
    fn including_directory_is_searched_first() {
        let (tokens, _) = run(
            "/root/lib/prog.cob",
            &src(&["COPY A."]),
            &[
                ("/root/lib/A.cpy", &src(&["DISPLAY L."])),
                ("/root/A.cpy", &src(&["DISPLAY R."])),
            ],
            &search(),
        );
        assert_eq!(texts(&tokens)[1], "L");
    }

    #[test]
    fn scan_root_precedes_copybooks_which_precede_copy() {
        let (tokens, _) = run(
            "/root/prog.cob",
            &src(&["COPY A."]),
            &[
                ("/root/A.cpy", &src(&["DISPLAY R."])),
                ("/root/copybooks/A.cpy", &src(&["DISPLAY B."])),
                ("/root/copy/A.cpy", &src(&["DISPLAY K."])),
            ],
            &search(),
        );
        assert_eq!(texts(&tokens)[1], "R");
        let (tokens, _) = run(
            "/root/prog.cob",
            &src(&["COPY A."]),
            &[
                ("/root/copybooks/A.cpy", &src(&["DISPLAY B."])),
                ("/root/copy/A.cpy", &src(&["DISPLAY K."])),
            ],
            &search(),
        );
        assert_eq!(texts(&tokens)[1], "B");
        let (tokens, _) = run(
            "/root/prog.cob",
            &src(&["COPY A."]),
            &[("/root/copy/A.cpy", &src(&["DISPLAY K."]))],
            &crate::preprocess::Search {
                scan_root: "/root".into(),
                copy_paths: vec!["/ext".into()],
            },
        );
        assert_eq!(texts(&tokens)[1], "K");
    }

    #[test]
    fn copy_paths_are_searched_last_in_the_given_order() {
        let search = crate::preprocess::Search {
            scan_root: "/root".into(),
            copy_paths: vec!["/ext1".into(), "/ext2".into()],
        };
        let (tokens, _) = run(
            "/root/prog.cob",
            &src(&["COPY A."]),
            &[
                ("/ext2/A.cpy", &src(&["DISPLAY E2."])),
                ("/ext1/A.cpy", &src(&["DISPLAY E1."])),
            ],
            &search,
        );
        assert_eq!(texts(&tokens)[1], "E1");
    }

    #[test]
    fn copybook_extensions_are_matched_in_lower_and_upper_case() {
        let (tokens, diags) = run(
            "/root/prog.cob",
            &src(&["COPY B.", "COPY C."]),
            &[
                ("/root/B.cbl", &src(&["DISPLAY B."])),
                ("/root/C.COB", &src(&["DISPLAY C."])),
            ],
            &search(),
        );
        assert_eq!(texts(&tokens), ["DISPLAY", "B", ".", "DISPLAY", "C", "."]);
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn missing_copybook_is_a_diagnostic_at_the_copy_and_the_scan_goes_on() {
        let (tokens, diags) = run(
            "/root/prog.cob",
            &src(&["COPY MISSING.", "STOP RUN."]),
            &[],
            &search(),
        );
        assert_eq!(texts(&tokens), ["STOP", "RUN", "."]);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(
            (diags[0].file.as_str(), diags[0].line, diags[0].col),
            ("/root/prog.cob", 1, 12)
        );
        assert!(diags[0].message.contains("MISSING"), "{diags:?}");
    }

    #[test]
    fn circular_copy_is_a_diagnostic_at_the_inner_copy() {
        let (tokens, diags) = run(
            "/root/prog.cob",
            &src(&["COPY A."]),
            &[("/root/A.cpy", &src(&["DISPLAY A.", "COPY A."]))],
            &search(),
        );
        assert_eq!(texts(&tokens), ["DISPLAY", "A", "."]);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(
            (diags[0].file.as_str(), diags[0].line, diags[0].col),
            ("/root/A.cpy", 2, 12)
        );
        assert!(diags[0].message.contains("circular"), "{diags:?}");
    }

    #[test]
    fn nesting_within_the_depth_limit_expands() {
        let owned = chain(crate::preprocess::MAX_DEPTH, "END");
        let files: Vec<(&str, &str)> = owned
            .iter()
            .map(|(p, t)| (p.as_str(), t.as_str()))
            .collect();
        let (tokens, diags) = run("/root/prog.cob", &src(&["COPY L1."]), &files, &search());
        assert!(texts(&tokens).contains(&"END"), "{tokens:?}");
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn nesting_past_the_depth_limit_is_a_diagnostic_and_the_scan_goes_on() {
        let owned = chain(crate::preprocess::MAX_DEPTH + 1, "FAR");
        let files: Vec<(&str, &str)> = owned
            .iter()
            .map(|(p, t)| (p.as_str(), t.as_str()))
            .collect();
        let (tokens, diags) = run(
            "/root/prog.cob",
            &src(&["COPY L1.", "STOP RUN."]),
            &files,
            &search(),
        );
        assert!(!texts(&tokens).contains(&"FAR"), "{tokens:?}");
        assert!(texts(&tokens).contains(&"RUN"), "{tokens:?}");
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert!(diags[0].message.contains("nests deeper"), "{diags:?}");
    }

    fn diag(message: &str) -> crate::diag::Diagnostic {
        crate::diag::Diagnostic {
            file: "prog.cob".to_string(),
            line: 12,
            col: 7,
            message: message.to_string(),
        }
    }

    #[test]
    fn diagnostic_prints_as_one_cobol_line() {
        let mut out = Vec::new();
        crate::diag::write_diagnostic(&mut out, &diag("unknown word FOO")).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "cobol: prog.cob:12:7: unknown word FOO\n"
        );
    }

    #[test]
    fn each_diagnostic_is_its_own_line_and_the_scan_order_is_kept() {
        let mut out = Vec::new();
        for message in ["first", "second"] {
            crate::diag::write_diagnostic(&mut out, &diag(message)).unwrap();
        }
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "cobol: prog.cob:12:7: first\ncobol: prog.cob:12:7: second\n"
        );
    }
}
