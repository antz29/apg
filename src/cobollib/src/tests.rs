//! Unit tests for the dialect table. The table is a single unit, so the tests live in
//! `tests::unit` alone (no `int` tier: nothing here wires two units together).

use crate::ast::{CallTarget, DataDivision, DataEntry, ProcedureDivision, Transfer};
use crate::dialect::{Dialect, Feature, FeatureLevel, Limits, SourceFormat, OO_WORDS};
use crate::lexer::{lex, Token, TokenKind};
use crate::parser::data::parse_data_division;
use crate::parser::procedure::parse_procedure_division;
use crate::source_map::SourceMap;

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

/// Lexes free-format `text` as `p.cob`, maps its tokens and parses its DATA DIVISION under cobol85.
fn parse_data(text: &str) -> Option<(DataDivision, Vec<crate::diag::Diagnostic>)> {
    let mut map = SourceMap::default();
    let file = map.add_file("p.cob");
    let (mut cursor, _) = lex("p.cob", text, crate::format::Layout::Free, Dialect::Cobol85);
    let mut tokens = Vec::new();
    while let Some(token) = cursor.bump() {
        tokens.push(token);
    }
    let mapped = map.map(file, tokens);
    parse_data_division(&map, Dialect::Cobol85, &mapped)
}

/// Lexes free-format `text` as `p.cob`, maps its tokens and parses its PROCEDURE DIVISION under cobol85.
fn parse_procedure(text: &str) -> Option<(ProcedureDivision, Vec<crate::diag::Diagnostic>)> {
    let mut map = SourceMap::default();
    let file = map.add_file("p.cob");
    let (mut cursor, _) = lex("p.cob", text, crate::format::Layout::Free, Dialect::Cobol85);
    let mut tokens = Vec::new();
    while let Some(token) = cursor.bump() {
        tokens.push(token);
    }
    let mapped = map.map(file, tokens);
    parse_procedure_division(&map, Dialect::Cobol85, &mapped)
}

/// Parses free-format `text` as `p.cob` under cobol85, with no copybooks.
fn parse_file(text: &str) -> (crate::ast::CompilationUnit, Vec<crate::diag::Diagnostic>) {
    let search = crate::preprocess::Search {
        scan_root: std::path::PathBuf::new(),
        copy_paths: Vec::new(),
    };
    crate::parser::parse_source(
        "p.cob",
        &format!(">>SOURCE FREE\n{text}"),
        Dialect::Cobol85,
        &search,
        &|_| None,
    )
}

/// The data-name text of each entry in order, `FILLER` for an unnamed one.
fn names(entries: &[DataEntry]) -> Vec<&str> {
    entries
        .iter()
        .map(|e| e.name.as_ref().map_or("FILLER", |n| n.text.as_str()))
        .collect()
}

mod unit {
    use super::*;
    use crate::format::Layout;

    mod parser_procedure {
        use super::*;

        #[test]
        fn paragraphs_hold_sentences_and_qualified_references() {
            let text = "PROCEDURE DIVISION.\nMAIN-PARA.\n    DISPLAY X.\n    STOP RUN.\n    MOVE A OF S TO B.\n";
            let (div, diags) = parse_procedure(text).unwrap();
            assert!(diags.is_empty());
            assert_eq!(div.paragraphs.len(), 1);
            let sentences = &div.paragraphs[0].sentences;
            assert_eq!(sentences.len(), 3);
            assert_eq!(sentences[2].statements[0].refs[0].qualifiers.len(), 1);
        }

        #[test]
        fn declaratives_and_sections_nest_their_paragraphs() {
            let text = "PROCEDURE DIVISION.\nDECLARATIVES.\nERR-SECTION SECTION.\n    USE AFTER STANDARD ERROR PROCEDURE ON F.\nERR-PARA.\n    DISPLAY X.\nEND DECLARATIVES.\nMAIN SECTION.\nMAIN-PARA.\n    STOP RUN.\n";
            let (div, diags) = parse_procedure(text).unwrap();
            assert!(diags.is_empty());
            assert_eq!(div.declaratives.len(), 1);
            assert_eq!(div.declaratives[0].name.text, "ERR-SECTION");
            assert_eq!(div.declaratives[0].paragraphs.len(), 1);
            assert_eq!(div.sections.len(), 1);
            assert_eq!(div.sections[0].name.text, "MAIN");
            assert_eq!(div.sections[0].paragraphs[0].name.text, "MAIN-PARA");
        }

        #[test]
        fn out_of_line_perform_keeps_its_thru_range() {
            let text = "PROCEDURE DIVISION.\nA-PARA.\n    PERFORM B-PARA THRU C-PARA 3 TIMES.\nB-PARA.\n    CONTINUE.\nC-PARA.\n    CONTINUE.\n";
            let (div, diags) = parse_procedure(text).unwrap();
            assert!(diags.is_empty());
            match &div.paragraphs[0].sentences[0].statements[0].transfer {
                Some(Transfer::Perform(Some(range))) => {
                    assert_eq!(range.from.name.text, "B-PARA");
                    assert_eq!(
                        range.thru.as_ref().map(|q| q.name.text.as_str()),
                        Some("C-PARA")
                    );
                }
                other => panic!("expected an out-of-line PERFORM, found {other:?}"),
            }
        }

        #[test]
        fn inline_perform_nests_an_if_with_its_else() {
            let text = "PROCEDURE DIVISION.\nA-PARA.\n    PERFORM VARYING I FROM 1 BY 1 UNTIL I > 5\n        IF I = 2 DISPLAY I ELSE DISPLAY J END-IF\n    END-PERFORM.\n";
            let (div, diags) = parse_procedure(text).unwrap();
            assert!(diags.is_empty());
            let perform = &div.paragraphs[0].sentences[0].statements[0];
            assert_eq!(perform.transfer, Some(Transfer::Perform(None)));
            assert!(perform.scope_end.is_some());
            assert_eq!(perform.children.len(), 1);
            let branch = &perform.children[0];
            assert!(branch.scope_end.is_some());
            assert_eq!(branch.children.len(), 2);
        }

        #[test]
        fn go_to_keeps_its_targets_and_an_unnamed_go_is_obsolete() {
            let text =
                "PROCEDURE DIVISION.\nA-PARA.\n    GO TO B-PARA C-PARA DEPENDING ON N.\n    GO.\n";
            let (div, diags) = parse_procedure(text).unwrap();
            assert_eq!(diags.len(), 1);
            let sentences = &div.paragraphs[0].sentences;
            match &sentences[0].statements[0].transfer {
                Some(Transfer::GoTo(targets)) => assert_eq!(targets.len(), 2),
                other => panic!("expected GO TO, found {other:?}"),
            }
            assert_eq!(sentences[0].statements[0].refs[0].name.text, "N");
            assert_eq!(
                sentences[1].statements[0].transfer,
                Some(Transfer::GoTo(Vec::new()))
            );
        }

        #[test]
        fn alter_is_obsolete_but_still_scanned() {
            let text = "PROCEDURE DIVISION.\nA-PARA.\n    ALTER B-PARA TO PROCEED TO C-PARA.\n";
            let (div, diags) = parse_procedure(text).unwrap();
            assert_eq!(diags.len(), 1);
            assert_eq!(div.paragraphs[0].sentences[0].statements.len(), 1);
        }

        #[test]
        fn entry_is_silent() {
            let (_, diags) =
                parse_procedure("PROCEDURE DIVISION.\nA-PARA.\n    ENTRY 'E' USING X.\n").unwrap();
            assert!(diags.is_empty());
        }

        #[test]
        fn call_takes_a_literal_or_an_identifier_target() {
            let text = "PROCEDURE DIVISION.\nA-PARA.\n    CALL 'SUB1' USING A.\n    CALL P-NAME.\n";
            let (div, diags) = parse_procedure(text).unwrap();
            assert!(diags.is_empty());
            let statements = &div.paragraphs[0].sentences;
            match &statements[0].statements[0].transfer {
                Some(Transfer::Call(CallTarget::Literal(name))) => assert_eq!(name.text, "SUB1"),
                other => panic!("expected a literal CALL, found {other:?}"),
            }
            match &statements[1].statements[0].transfer {
                Some(Transfer::Call(CallTarget::Identifier(name))) => {
                    assert_eq!(name.name.text, "P-NAME")
                }
                other => panic!("expected an identifier CALL, found {other:?}"),
            }
        }

        #[test]
        fn sort_keeps_its_input_and_output_procedures() {
            let text = "PROCEDURE DIVISION.\nA-PARA.\n    SORT S-FILE ON ASCENDING KEY K INPUT PROCEDURE IN-PARA OUTPUT PROCEDURE IS OUT-PARA THRU OUT-END.\n";
            let (div, diags) = parse_procedure(text).unwrap();
            assert!(diags.is_empty());
            match &div.paragraphs[0].sentences[0].statements[0].transfer {
                Some(Transfer::SortMerge { input, output }) => {
                    assert_eq!(
                        input.as_ref().map(|r| r.from.name.text.as_str()),
                        Some("IN-PARA")
                    );
                    let out = output.as_ref().unwrap();
                    assert_eq!(out.from.name.text, "OUT-PARA");
                    assert_eq!(
                        out.thru.as_ref().map(|q| q.name.text.as_str()),
                        Some("OUT-END")
                    );
                }
                other => panic!("expected SORT, found {other:?}"),
            }
        }

        #[test]
        fn a_stray_scope_terminator_costs_only_its_sentence() {
            let text = "PROCEDURE DIVISION.\nA-PARA.\n    MOVE 1 TO X END-IF.\n    DISPLAY Y.\nB-PARA.\n    DISPLAY Z.\n";
            let (div, diags) = parse_procedure(text).unwrap();
            assert_eq!(diags.len(), 1);
            assert_eq!(div.paragraphs.len(), 2);
            assert_eq!(div.paragraphs[0].sentences.len(), 2);
            assert_eq!(div.paragraphs[0].sentences[0].statements.len(), 1);
        }

        #[test]
        fn unknown_verb_is_a_sentence_diagnostic() {
            let (div, diags) =
                parse_procedure("PROCEDURE DIVISION.\nA-PARA.\n    FROB X.\n").unwrap();
            assert_eq!(diags.len(), 1);
            assert!(div.paragraphs[0].sentences[0].statements.is_empty());
        }

        #[test]
        fn text_without_the_division_header_is_none() {
            assert!(parse_procedure("DATA DIVISION.\n").is_none());
        }
    }

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
    fn period_followed_by_text_is_part_of_the_token() {
        let text = at(8, "PIC ZZZ.ZZ. 9.99. 9.X 1.");
        let (tokens, _) = lex_all(&text, fixed(), Dialect::Cobol85);
        assert_eq!(
            shape(&tokens),
            [
                (TokenKind::Word, "PIC"),
                (TokenKind::Word, "ZZZ.ZZ"),
                (TokenKind::Period, "."),
                (TokenKind::Numeric, "9.99"),
                (TokenKind::Period, "."),
                (TokenKind::Word, "9.X"),
                (TokenKind::Numeric, "1"),
                (TokenKind::Period, ".")
            ]
        );
    }

    #[test]
    fn slash_in_column_seven_is_a_comment_line() {
        let text = [at(7, "/ PAGE EJECT"), at(8, "DISPLAY C.")].join("\n");
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

    mod parser_data {
        use super::*;

        #[test]
        fn levels_nest_by_number() {
            let (division, diags) = parse_data(
                "DATA DIVISION. WORKING-STORAGE SECTION. 01 REC. 05 A PIC X. 05 B. 10 C PIC 9. 05 D PIC X. 01 OTHER PIC X.",
            )
            .unwrap();
            assert!(diags.is_empty(), "{diags:?}");
            let entries = &division.sections[0].entries;
            assert_eq!(names(entries), ["REC", "OTHER"]);
            let rec = &entries[0].children;
            assert_eq!(names(rec), ["A", "B", "D"]);
            assert_eq!(names(&rec[1].children), ["C"]);
            assert_eq!(rec[1].children[0].level, 10);
        }

        #[test]
        fn level_77_66_and_88_attach_as_the_spec_places_them() {
            let (division, diags) = parse_data(
                "DATA DIVISION. WORKING-STORAGE SECTION. 77 W PIC X. 01 R. 05 F PIC X. 88 F-ON VALUE 'Y'. 66 ALIAS RENAMES F.",
            )
            .unwrap();
            assert!(diags.is_empty(), "{diags:?}");
            let entries = &division.sections[0].entries;
            assert_eq!(names(entries), ["W", "R"]);
            assert_eq!(entries[0].level, 77);
            let record = &entries[1].children;
            assert_eq!(names(record), ["F", "ALIAS"]);
            assert!(record[1].renames);
            assert_eq!(names(&record[0].children), ["F-ON"]);
            assert_eq!(record[0].children[0].level, 88);
        }

        #[test]
        fn filler_redefines_and_occurs_are_recorded() {
            let (division, diags) = parse_data(
                "DATA DIVISION. WORKING-STORAGE SECTION. 01 R. 05 FILLER PIC X. 05 X1 REDEFINES FILLER PIC 9. 05 T OCCURS 3 TIMES PIC X VALUE 'A'.",
            )
            .unwrap();
            assert!(diags.is_empty(), "{diags:?}");
            let kids = &division.sections[0].entries[0].children;
            assert_eq!(names(kids), ["FILLER", "X1", "T"]);
            assert!(kids[0].name.is_none());
            assert!(kids[1].redefines);
            assert!(kids[2].occurs);
            assert_eq!(kids[2].pic.as_ref().map(|p| p.text.as_str()), Some("X"));
        }

        #[test]
        fn picture_strings_are_read_whole() {
            let (division, diags) = parse_data(
                "DATA DIVISION. WORKING-STORAGE SECTION. 01 R. 05 A PIC S9(5)V99 VALUE 0. 05 E PIC ZZ,ZZ9.99.",
            )
            .unwrap();
            assert!(diags.is_empty(), "{diags:?}");
            let kids = &division.sections[0].entries[0].children;
            assert_eq!(
                kids[0].pic.as_ref().map(|p| p.text.as_str()),
                Some("S9(5)V99")
            );
            assert_eq!(
                kids[1].pic.as_ref().map(|p| p.text.as_str()),
                Some("ZZ,ZZ9.99")
            );
        }

        #[test]
        fn picture_over_the_dialect_limit_is_a_diagnostic_and_keeps_the_entry() {
            let text = format!(
                "DATA DIVISION. WORKING-STORAGE SECTION. 01 R PIC {}.",
                "9".repeat(31)
            );
            let (division, diags) = parse_data(&text).unwrap();
            assert_eq!(diags.len(), 1, "{diags:?}");
            assert!(
                diags[0].message.contains("over the dialect limit of 30"),
                "{diags:?}"
            );
            assert_eq!(names(&division.sections[0].entries), ["R"]);
        }

        #[test]
        fn a_broken_entry_loses_only_itself() {
            let (division, diags) = parse_data(
                "DATA DIVISION. WORKING-STORAGE SECTION. 01 R. 05 A PIC. 05 B PIC X. 01 S PIC X.",
            )
            .unwrap();
            assert_eq!(diags.len(), 1, "{diags:?}");
            let entries = &division.sections[0].entries;
            assert_eq!(names(entries), ["R", "S"]);
            assert_eq!(names(&entries[0].children), ["B"]);
        }

        #[test]
        fn sections_are_kept_and_screen_report_communication_are_skipped() {
            let (division, diags) = parse_data(
                "DATA DIVISION. FILE SECTION. FD F1 RECORD CONTAINS 80. 01 FR PIC X. WORKING-STORAGE SECTION. 01 W PIC X. SCREEN SECTION. 01 S PIC X VALUE 'a'. LINKAGE SECTION. 01 L PIC X.",
            )
            .unwrap();
            assert!(diags.is_empty(), "{diags:?}");
            let kinds: Vec<_> = division.sections.iter().map(|s| s.kind).collect();
            assert_eq!(
                kinds,
                [
                    crate::ast::DataSectionKind::File,
                    crate::ast::DataSectionKind::WorkingStorage,
                    crate::ast::DataSectionKind::Linkage
                ]
            );
            let entry_names: Vec<Vec<&str>> = division
                .sections
                .iter()
                .map(|s| names(&s.entries))
                .collect();
            assert_eq!(entry_names, [vec!["FR"], vec!["W"], vec!["L"]]);
        }

        #[test]
        fn an_entry_missing_its_period_is_kept_with_a_diagnostic() {
            let (division, diags) = parse_data(
                "DATA DIVISION. WORKING-STORAGE SECTION. 01 A PIC X LINKAGE SECTION. 01 L PIC X.",
            )
            .unwrap();
            assert_eq!(diags.len(), 1, "{diags:?}");
            assert_eq!(names(&division.sections[0].entries), ["A"]);
            assert_eq!(division.sections.len(), 2);
        }

        #[test]
        fn text_before_the_first_section_is_one_diagnostic() {
            let (division, diags) = parse_data(
                "DATA DIVISION. 01 X PIC X. 05 Y PIC X. WORKING-STORAGE SECTION. 01 W PIC X.",
            )
            .unwrap();
            assert_eq!(diags.len(), 1, "{diags:?}");
            assert_eq!(names(&division.sections[0].entries), ["W"]);
        }

        #[test]
        fn text_that_is_not_a_data_division_parses_to_none() {
            assert!(parse_data("PROCEDURE DIVISION. STOP RUN.").is_none());
        }
    }

    mod parser_entry {
        use super::*;
        use std::path::{Path, PathBuf};

        #[test]
        fn nested_program_is_a_child_and_end_program_closes_each() {
            let (unit, diags) = parse_file(
                "IDENTIFICATION DIVISION.\nPROGRAM-ID. OUTER.\nPROCEDURE DIVISION.\nMAIN.\n    DISPLAY 'X'.\nIDENTIFICATION DIVISION.\nPROGRAM-ID. INNER.\nPROCEDURE DIVISION.\nMAIN.\n    STOP RUN.\nEND PROGRAM INNER.\nEND PROGRAM OUTER.\n",
            );
            assert!(diags.is_empty(), "{diags:?}");
            assert_eq!(unit.programs.len(), 1);
            let outer = &unit.programs[0];
            assert_eq!(outer.name.text, "OUTER");
            assert_eq!(outer.nested[0].name.text, "INNER");
            assert!(outer.procedure.is_some() && outer.nested[0].procedure.is_some());
        }

        #[test]
        fn programs_in_sequence_keep_their_own_divisions() {
            let (unit, diags) = parse_file(
                "IDENTIFICATION DIVISION.\nPROGRAM-ID. ONE.\nENVIRONMENT DIVISION.\nPROCEDURE DIVISION.\nMAIN.\n    STOP RUN.\nEND PROGRAM ONE.\nIDENTIFICATION DIVISION.\nPROGRAM-ID. TWO.\nDATA DIVISION.\nWORKING-STORAGE SECTION.\n01 A PIC X.\nEND PROGRAM TWO.\n",
            );
            assert!(diags.is_empty(), "{diags:?}");
            assert_eq!(unit.programs.len(), 2);
            assert!(unit.programs[0].environment.is_some());
            assert!(unit.programs[1].data.is_some() && unit.programs[1].procedure.is_none());
        }

        #[test]
        fn program_without_end_program_is_a_sibling_not_nested() {
            let (unit, diags) = parse_file(
                "IDENTIFICATION DIVISION.\nPROGRAM-ID. ONE.\nPROCEDURE DIVISION.\nMAIN.\n    STOP RUN.\nIDENTIFICATION DIVISION.\nPROGRAM-ID. TWO.\nPROCEDURE DIVISION.\nMAIN.\n    STOP RUN.\n",
            );
            assert!(diags.is_empty(), "{diags:?}");
            assert_eq!(unit.programs.len(), 2);
            assert!(unit.programs.iter().all(|p| p.nested.is_empty()));
        }

        #[test]
        fn declarations_are_kept_around_a_syntax_error() {
            let (unit, diags) = parse_file(
                "IDENTIFICATION DIVISION.\nPROGRAM-ID. P.\nDATA DIVISION.\nWORKING-STORAGE SECTION.\n01 A PIC X.\nPROCEDURE DIVISION.\nP1.\n    FOO BAR.\nP2.\n    DISPLAY 'X'.\nEND PROGRAM P.\n",
            );
            assert!(!diags.is_empty());
            let program = &unit.programs[0];
            let data = program.data.as_ref().unwrap();
            assert_eq!(names(&data.sections[0].entries), ["A"]);
            assert_eq!(program.procedure.as_ref().unwrap().paragraphs.len(), 2);
        }

        #[test]
        fn program_without_program_id_is_one_diagnostic_naming_the_file() {
            let (unit, diags) = parse_file(
                "IDENTIFICATION DIVISION.\nAUTHOR. X.\nPROCEDURE DIVISION.\n    STOP RUN.\n",
            );
            assert!(unit.programs.is_empty());
            assert_eq!(diags.len(), 1, "{diags:?}");
            assert_eq!(diags[0].file, "p.cob");
        }

        #[test]
        fn copybook_text_is_part_of_the_program() {
            let search = crate::preprocess::Search {
                scan_root: PathBuf::new(),
                copy_paths: Vec::new(),
            };
            let lookup = |p: &Path| {
                p.file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with("CB"))
                    .then(|| "       01 W PIC X.\n".to_string())
            };
            let (unit, diags) = crate::parser::parse_source(
                "p.cob",
                ">>SOURCE FREE\nIDENTIFICATION DIVISION.\nPROGRAM-ID. P.\nDATA DIVISION.\nWORKING-STORAGE SECTION.\nCOPY CB.\nPROCEDURE DIVISION.\nEND PROGRAM P.\n",
                Dialect::Cobol85,
                &search,
                &lookup,
            );
            assert!(diags.is_empty(), "{diags:?}");
            let data = unit.programs[0].data.as_ref().unwrap();
            assert_eq!(names(&data.sections[0].entries), ["W"]);
        }
    }
}
