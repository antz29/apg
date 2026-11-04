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
        fn end_prefixed_user_words_are_paragraphs_and_perform_targets() {
            let text = "PROCEDURE DIVISION.\nA-PARA.\n    PERFORM END-ROUTINE THRU END-ROUTINE-13.\nEND-ROUTINE.\n    DISPLAY X.\nEND-ROUTINE-13.\n    DISPLAY Y.\n";
            let (div, diags) = parse_procedure(text).unwrap();
            assert!(diags.is_empty(), "{diags:?}");
            let names: Vec<&str> = div
                .paragraphs
                .iter()
                .map(|p| p.name.text.as_str())
                .collect();
            assert_eq!(names, ["A-PARA", "END-ROUTINE", "END-ROUTINE-13"]);
            match &div.paragraphs[0].sentences[0].statements[0].transfer {
                Some(Transfer::Perform(Some(range))) => {
                    assert_eq!(range.from.name.text, "END-ROUTINE");
                    assert_eq!(
                        range.thru.as_ref().map(|q| q.name.text.as_str()),
                        Some("END-ROUTINE-13")
                    );
                }
                other => panic!("expected an out-of-line PERFORM, found {other:?}"),
            }
        }

        #[test]
        fn write_at_end_of_page_is_a_write_phrase() {
            let text = "PROCEDURE DIVISION.\nA-PARA.\n    WRITE R AT END-OF-PAGE DISPLAY \"X\".\n";
            let (div, diags) = parse_procedure(text).unwrap();
            assert!(diags.is_empty(), "{diags:?}");
            assert_eq!(div.paragraphs[0].sentences[0].statements.len(), 1);
        }

        #[test]
        fn end_if_still_closes_its_if() {
            let text = "PROCEDURE DIVISION.\nA-PARA.\n    IF X = 1 DISPLAY Y END-IF DISPLAY Z.\n";
            let (div, diags) = parse_procedure(text).unwrap();
            assert!(diags.is_empty(), "{diags:?}");
            let statements = &div.paragraphs[0].sentences[0].statements;
            assert_eq!(statements.len(), 2);
            assert!(statements[0].scope_end.is_some());
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
        fn optional_phrase_words_open_their_branches() {
            let cases = [
                ("READ F END MOVE 1 TO X END-READ.", 1),
                ("READ F NOT AT END MOVE 1 TO X END-READ.", 1),
                (
                    "WRITE R INVALID GO TO A-PARA NOT INVALID MOVE 1 TO X END-WRITE.",
                    2,
                ),
                ("WRITE R NOT EOP DISPLAY X END-WRITE.", 1),
                ("CALL 'S' USING A OVERFLOW DISPLAY X END-CALL.", 1),
                ("CALL 'S' USING A NOT ON EXCEPTION DISPLAY X END-CALL.", 1),
                ("ADD 1 TO X SIZE ERROR DISPLAY Y END-ADD.", 1),
            ];
            for (stmt, children) in cases {
                let text = format!("PROCEDURE DIVISION.\nA-PARA.\n    {stmt}\n");
                let (div, diags) = parse_procedure(&text).unwrap();
                assert!(diags.is_empty(), "{stmt}: {diags:?}");
                let statement = &div.paragraphs[0].sentences[0].statements;
                assert_eq!(statement.len(), 1, "{stmt}");
                assert_eq!(statement[0].children.len(), children, "{stmt}");
            }
        }

        #[test]
        fn search_takes_when_conditions_and_its_end_search() {
            let text = "PROCEDURE DIVISION.\nA-PARA.\n    SEARCH GRP VARYING N1 WHEN E (N1) = L1 MOVE \"AA\" TO L2 WHEN E (N1) = L3 DISPLAY X END-SEARCH.\n    SEARCH ALL T END GO TO A-PARA WHEN T1 EQUAL TO \"05\" AND T2 EQUAL \"07\" DISPLAY Y.\n";
            let (div, diags) = parse_procedure(text).unwrap();
            assert!(diags.is_empty(), "{diags:?}");
            let sentences = &div.paragraphs[0].sentences;
            assert_eq!(sentences.len(), 2);
            let search = &sentences[0].statements[0];
            assert!(search.scope_end.is_some());
            assert_eq!(search.children.len(), 2);
            assert!(search.refs.iter().any(|r| r.name.text == "L3"));
            assert_eq!(sentences[1].statements[0].children.len(), 2);
        }

        #[test]
        fn all_digit_section_and_paragraph_names_are_procedure_names() {
            let text = "PROCEDURE DIVISION.\n00 SECTION 00.\nA-PARA.\n    GO TO 3 4 DEPENDING ON N.\n3.\n    PERFORM 3 THRU 4.\n4.\n    DISPLAY X.\n";
            let (div, diags) = parse_procedure(text).unwrap();
            assert!(diags.is_empty(), "{diags:?}");
            assert_eq!(div.sections[0].name.text, "00");
            let names: Vec<&str> = div.sections[0]
                .paragraphs
                .iter()
                .map(|p| p.name.text.as_str())
                .collect();
            assert_eq!(names, ["A-PARA", "3", "4"]);
            match &div.sections[0].paragraphs[0].sentences[0].statements[0].transfer {
                Some(Transfer::GoTo(targets)) => {
                    assert_eq!(targets.len(), 2);
                    assert_eq!(targets[0].name.text, "3");
                }
                other => panic!("expected GO TO, found {other:?}"),
            }
            match &div.sections[0].paragraphs[1].sentences[0].statements[0].transfer {
                Some(Transfer::Perform(Some(range))) => {
                    assert_eq!(range.from.name.text, "3");
                    assert_eq!(range.thru.as_ref().map(|q| q.name.text.as_str()), Some("4"));
                }
                other => panic!("expected an out-of-line PERFORM, found {other:?}"),
            }
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
    fn indicators_accepted_including_slash() {
        let ok = [at(7, "*"), at(7, "/ PAGE EJECT"), at(7, "D"), at(7, "-")].join("\n");
        let (layout, diags) = crate::format::check("p.cob", &ok, Dialect::Cobol2002);
        assert_eq!(layout, Layout::Source(SourceFormat::Fixed));
        assert!(diags.is_empty(), "{diags:?}");
        let dollar = at(7, "$ comment");
        let (_, diags) = crate::format::check("p.cob", &dollar, Dialect::Cobol2002);
        assert_eq!(diags.len(), 1, "{diags:?}");
    }

    #[test]
    fn slash_line_draws_no_area_a_diagnostic() {
        let text = [at(8, "PROCEDURE DIVISION."), at(7, "/ PAGE EJECT")].join("\n");
        let (_, diags) = crate::format::check("p.cob", &text, Dialect::Cobol85);
        assert!(diags.is_empty(), "{diags:?}");
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
    fn area_b_one_word_entry_after_program_id_draws_no_diagnostic() {
        let text = [at(8, "PROGRAM-ID. PROG."), at(12, "ST101A.")].join("\n");
        let (_, diags) = crate::format::check("p.cob", &text, Dialect::Cobol85);
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn area_b_program_id_is_still_a_diagnostic() {
        let text = [at(8, "IDENTIFICATION DIVISION."), at(12, "PROGRAM-ID.")].join("\n");
        let (_, diags) = crate::format::check("p.cob", &text, Dialect::Cobol85);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].line, 2);
    }

    #[test]
    fn user_paragraph_named_end_routine_in_area_a_is_a_paragraph_header() {
        let text = [at(8, "PROCEDURE DIVISION."), at(8, "END-ROUTINE.")].join("\n");
        let (_, diags) = crate::format::check("p.cob", &text, Dialect::Cobol85);
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn user_paragraph_named_end_routine_in_area_b_is_a_diagnostic() {
        let text = [at(8, "PROCEDURE DIVISION."), at(12, "END-ROUTINE.")].join("\n");
        let (_, diags) = crate::format::check("p.cob", &text, Dialect::Cobol85);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].line, 2);
    }

    #[test]
    fn scope_terminator_in_area_b_draws_no_diagnostic() {
        let text = [at(8, "PROCEDURE DIVISION."), at(12, "END-IF.")].join("\n");
        let (_, diags) = crate::format::check("p.cob", &text, Dialect::Cobol85);
        assert!(diags.is_empty(), "{diags:?}");
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
    fn header_words_spanning_lines_are_one_header() {
        let ok = [
            at(8, "IDENTIFICATION DIVISION."),
            at(11, "DATA"),
            at(12, "DIVISION."),
        ]
        .join("\n");
        let (_, diags) = crate::format::check("p.cob", &ok, Dialect::Cobol85);
        assert!(diags.is_empty(), "{diags:?}");
        let bad = [at(12, "DATA"), at(12, "DIVISION.")].join("\n");
        let (_, diags) = crate::format::check("p.cob", &bad, Dialect::Cobol85);
        assert_eq!(
            diags.iter().map(|d| d.line).collect::<Vec<_>>(),
            [1],
            "{diags:?}"
        );
    }

    #[test]
    fn separator_period_after_a_space_closes_the_name() {
        let ok = [
            at(8, "PROCEDURE DIVISION."),
            at(8, "RWRT-INIT-GF-01-R ."),
            at(8, "A SECTION ."),
            at(12, "MOVE X TO Y."),
        ]
        .join("\n");
        let (_, diags) = crate::format::check("p.cob", &ok, Dialect::Cobol85);
        assert!(diags.is_empty(), "{diags:?}");
        let bad = [at(8, "PROCEDURE DIVISION."), at(12, "B .")].join("\n");
        let (_, diags) = crate::format::check("p.cob", &bad, Dialect::Cobol85);
        assert_eq!(
            diags.iter().map(|d| d.line).collect::<Vec<_>>(),
            [2],
            "{diags:?}"
        );
    }

    #[test]
    fn all_digit_paragraph_name_in_procedure_is_a_paragraph() {
        let ok = [at(8, "PROCEDURE DIVISION."), at(8, "4.")].join("\n");
        let (_, diags) = crate::format::check("p.cob", &ok, Dialect::Cobol85);
        assert!(diags.is_empty(), "{diags:?}");
        let bad = [at(8, "PROCEDURE DIVISION."), at(12, "4.")].join("\n");
        let (_, diags) = crate::format::check("p.cob", &bad, Dialect::Cobol85);
        assert_eq!(
            diags.iter().map(|d| d.line).collect::<Vec<_>>(),
            [2],
            "{diags:?}"
        );
    }

    #[test]
    fn copy_and_replace_may_start_in_area_a_or_b() {
        let text = [
            at(8, "IDENTIFICATION DIVISION."),
            at(8, "REPLACE OFF."),
            at(8, "COPY  K1W04."),
            at(12, "COPY K2."),
        ]
        .join("\n");
        let (_, diags) = crate::format::check("p.cob", &text, Dialect::Cobol85);
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn comment_entry_lines_up_to_the_next_area_a_line_draw_nothing() {
        let text = [
            at(8, "IDENTIFICATION DIVISION."),
            at(8, "AUTHOR."),
            at(12, "FEDERAL COMPILER TESTING CENTRE."),
            at(12, "CONFIGURATION SECTION."),
            at(12, "PROCEDURE DIVISION."),
            at(12, "MAIN-PARA."),
            at(8, "ENVIRONMENT DIVISION."),
        ]
        .join("\n");
        let (_, diags) = crate::format::check("p.cob", &text, Dialect::Cobol85);
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn genuine_statement_in_area_a_after_a_sentence_end_is_flagged() {
        let text = [
            at(8, "PROCEDURE DIVISION."),
            at(12, "DISPLAY X."),
            at(8, "MOVE X TO Y."),
        ]
        .join("\n");
        let (_, diags) = crate::format::check("p.cob", &text, Dialect::Cobol85);
        assert_eq!(
            (diags.len(), diags[0].line, diags[0].col),
            (1, 3, 8),
            "{diags:?}"
        );
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
    fn author_comment_entry_with_unmatched_quote_yields_no_tokens_or_diagnostic() {
        let text = [
            at(8, "AUTHOR."),
            at(12, "FRED \"UNCLOSED"),
            at(8, "ENVIRONMENT DIVISION."),
        ]
        .join("\n");
        let (tokens, diags) = lex_all(&text, fixed(), Dialect::Cobol85);
        let texts: Vec<&str> = tokens.iter().map(|t| t.text.as_str()).collect();
        assert_eq!(texts, ["AUTHOR", ".", "ENVIRONMENT", "DIVISION", "."]);
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn remarks_comment_entry_with_area_b_division_header_yields_nothing() {
        let text = [
            at(8, "REMARKS."),
            at(12, "PROCEDURE DIVISION."),
            at(12, "DISPLAY X."),
            at(8, "PROCEDURE DIVISION."),
        ]
        .join("\n");
        let (tokens, _) = lex_all(&text, fixed(), Dialect::Cobol85);
        let texts: Vec<&str> = tokens.iter().map(|t| t.text.as_str()).collect();
        assert_eq!(texts, ["REMARKS", ".", "PROCEDURE", "DIVISION", "."]);
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
    fn continued_literal_keeps_the_trailing_blanks_of_the_continued_line() {
        let text = [at(8, "DISPLAY \"AB  "), format!("{}\"CD\".", at(7, "-"))].join("\n");
        let (tokens, diags) = lex_all(&text, fixed(), Dialect::Cobol85);
        assert_eq!(
            shape(&tokens),
            [
                (TokenKind::Word, "DISPLAY"),
                (TokenKind::Alnum, "AB  CD"),
                (TokenKind::Period, ".")
            ]
        );
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn continued_word_joins_across_blanks_at_both_ends_of_the_split() {
        let continued = |first: &str, second: &str| {
            let text = [
                format!("{}{:<65}", " ".repeat(7), first),
                format!("{}{second}", at(7, "-")),
            ]
            .join("\n");
            let (tokens, _) = lex_all(&text, fixed(), Dialect::Cobol85);
            tokens
        };
        let tokens = continued("PROCEDURE", "    DIVISION.");
        assert_eq!(
            shape(&tokens),
            [
                (TokenKind::Word, "PROCEDUREDIVISION"),
                (TokenKind::Period, ".")
            ]
        );
        assert_eq!((tokens[0].line, tokens[0].col), (1, 8));
        assert_eq!(
            shape(&continued("CONT-", "  A")),
            [(TokenKind::Word, "CONT-A")]
        );
        assert_eq!(
            shape(&continued("PIC", "  TURE")),
            [(TokenKind::Word, "PICTURE")]
        );
    }

    #[test]
    fn continued_picture_string_joins_like_its_unbroken_text() {
        let text = [at(8, "PIC S9("), format!("{}  6)V9(6).", at(7, "-"))].join("\n");
        let (continued, _) = lex_all(&text, fixed(), Dialect::Cobol85);
        let (single, _) = lex_all(&at(8, "PIC S9(6)V9(6)."), fixed(), Dialect::Cobol85);
        assert_eq!(shape(&continued), shape(&single));
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
    fn comma_or_semicolon_before_a_space_or_line_end_is_no_token() {
        let text = [at(8, "MOVE A, B; C,"), at(8, "D.")].join("\n");
        let (tokens, _) = lex_all(&text, fixed(), Dialect::Cobol85);
        assert_eq!(
            shape(&tokens),
            [
                (TokenKind::Word, "MOVE"),
                (TokenKind::Word, "A"),
                (TokenKind::Word, "B"),
                (TokenKind::Word, "C"),
                (TokenKind::Word, "D"),
                (TokenKind::Period, ".")
            ]
        );
    }

    #[test]
    fn picture_string_after_pic_is_one_token_and_a_final_period_separates() {
        let text = [
            at(8, "01 A PIC +++++.++. 01 B PICTURE IS --,---.--."),
            at(8, "01 C PIC $.** VALUE ZERO. 02 D PICTURE ***,***.**."),
        ]
        .join("\n");
        let (tokens, _) = lex_all(&text, fixed(), Dialect::Cobol85);
        assert_eq!(
            shape(&tokens),
            [
                (TokenKind::Numeric, "01"),
                (TokenKind::Word, "A"),
                (TokenKind::Word, "PIC"),
                (TokenKind::Word, "+++++.++"),
                (TokenKind::Period, "."),
                (TokenKind::Numeric, "01"),
                (TokenKind::Word, "B"),
                (TokenKind::Word, "PICTURE"),
                (TokenKind::Word, "--,---.--"),
                (TokenKind::Period, "."),
                (TokenKind::Numeric, "01"),
                (TokenKind::Word, "C"),
                (TokenKind::Word, "PIC"),
                (TokenKind::Word, "$.**"),
                (TokenKind::Word, "VALUE"),
                (TokenKind::Word, "ZERO"),
                (TokenKind::Period, "."),
                (TokenKind::Numeric, "02"),
                (TokenKind::Word, "D"),
                (TokenKind::Word, "PICTURE"),
                (TokenKind::Word, "***,***.**"),
                (TokenKind::Period, ".")
            ]
        );
    }

    #[test]
    fn period_ending_a_continued_number_stays_inside_it() {
        let text = [at(8, "MOVE 12."), format!("{}7 TO X.", at(7, "-"))].join("\n");
        let (tokens, diags) = lex_all(&text, fixed(), Dialect::Cobol85);
        assert_eq!(
            shape(&tokens),
            [
                (TokenKind::Word, "MOVE"),
                (TokenKind::Numeric, "12.7"),
                (TokenKind::Word, "TO"),
                (TokenKind::Word, "X"),
                (TokenKind::Period, ".")
            ]
        );
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn quote_in_last_column_pairs_with_the_continuation_reopening_quote() {
        let text = [
            at(8, &format!("{:<50}\"ABCDEFGHIJKLM\"", "DISPLAY")),
            format!("{}    \"\"9K(L)M>N<O PQRSTUVWXYZ\".", at(7, "-")),
        ]
        .join("\n");
        let (tokens, diags) = lex_all(&text, fixed(), Dialect::Cobol85);
        assert_eq!(
            shape(&tokens),
            [
                (TokenKind::Word, "DISPLAY"),
                (TokenKind::Alnum, "ABCDEFGHIJKLM\"9K(L)M>N<O PQRSTUVWXYZ"),
                (TokenKind::Period, ".")
            ]
        );
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn continued_number_with_blanks_after_the_dash_and_a_period_at_line_end() {
        let cont = |body: &str| format!("{}{:<65}", at(7, "-"), body);
        let text = [
            format!("{}{:<65}", " ".repeat(7), "MOVE -"),
            cont("             9"),
            cont("             9"),
            cont("             ."),
            cont("             7 TO X."),
        ]
        .join("\n");
        let (tokens, diags) = lex_all(&text, fixed(), Dialect::Cobol85);
        assert_eq!(
            shape(&tokens),
            [
                (TokenKind::Word, "MOVE"),
                (TokenKind::Numeric, "-99.7"),
                (TokenKind::Word, "TO"),
                (TokenKind::Word, "X"),
                (TokenKind::Period, ".")
            ]
        );
        assert!(diags.is_empty(), "{diags:?}");
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
        let (tokens, _) = lex_all(&at(8, "==A==,B;(C)"), fixed(), Dialect::Cobol85);
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
    fn replace_pseudo_text_may_span_lines() {
        let text = [
            at(8, "REPLACE ==DISPLAY"),
            at(8, "  A== BY ==SHOW=="),
            at(8, "  ."),
            at(8, "DISPLAY A."),
        ]
        .join("\n");
        let (tokens, diags) = replaced(&text);
        assert_eq!(
            shape(&tokens),
            [(TokenKind::Word, "SHOW"), (TokenKind::Period, ".")]
        );
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn replace_pseudo_text_may_hold_a_separator_period_and_empty_by_deletes_it() {
        let (tokens, diags) = replaced(&at(8, "REPLACE ==A. == BY ====. DISPLAY X A. DISPLAY B."));
        assert_eq!(
            shape(&tokens),
            [
                (TokenKind::Word, "DISPLAY"),
                (TokenKind::Word, "X"),
                (TokenKind::Word, "DISPLAY"),
                (TokenKind::Word, "B"),
                (TokenKind::Period, ".")
            ]
        );
        assert!(diags.is_empty(), "{diags:?}");
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
    fn copy_replacing_keeps_periods_inside_pseudo_text_and_empty_by() {
        let (tokens, diags) = run(
            "/root/prog.cob",
            &src(&["COPY A REPLACING ==A. == BY ====.", "STOP RUN."]),
            &[("/root/A.cpy", &src(&["DISPLAY A. B."]))],
            &search(),
        );
        assert_eq!(texts(&tokens), ["DISPLAY", "B", ".", "STOP", "RUN", "."]);
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn copy_replacing_takes_multi_line_pseudo_text() {
        let (tokens, diags) = run(
            "/root/prog.cob",
            &src(&["COPY A REPLACING ==DISPLAY X. ==", "BY ==DISPLAY Y. ==."]),
            &[("/root/A.cpy", &src(&["DISPLAY X."]))],
            &search(),
        );
        assert_eq!(texts(&tokens), ["DISPLAY", "Y", "."]);
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn copy_replacing_takes_qualified_identifier_operands() {
        let (tokens, diags) = run(
            "/root/prog.cob",
            &src(&["COPY A REPLACING X OF Q BY Y IN R COPY-W BY Z."]),
            &[("/root/A.cpy", &src(&["DISPLAY X OF Q COPY-W."]))],
            &search(),
        );
        assert_eq!(texts(&tokens), ["DISPLAY", "Y", "IN", "R", "Z", "."]);
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn copy_replacing_takes_literal_pairs() {
        let (tokens, diags) = run(
            "/root/prog.cob",
            &src(&["COPY A REPLACING 'X' BY 'Y'."]),
            &[("/root/A.cpy", &src(&["DISPLAY 'X'."]))],
            &search(),
        );
        assert_eq!(texts(&tokens), ["DISPLAY", "Y", "."]);
        assert!(diags.is_empty(), "{diags:?}");
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

    mod source_map {
        use super::*;
        use crate::format::Layout;

        #[test]
        fn set_layout_then_read_it_back() {
            let mut map = SourceMap::default();
            let file = map.add_file("p.cob");
            map.set_layout(file, Layout::Free);
            assert_eq!(map.layout(file), Some(Layout::Free));
        }

        #[test]
        fn unset_file_has_no_layout() {
            let mut map = SourceMap::default();
            let file = map.add_file("p.cob");
            assert_eq!(map.layout(file), None);
        }

        #[test]
        fn two_files_keep_their_own_layouts() {
            let mut map = SourceMap::default();
            let first = map.add_file("a.cob");
            let second = map.add_file("b.cpy");
            map.set_layout(second, Layout::Free);
            map.set_layout(first, fixed());
            assert_eq!(map.layout(first), Some(fixed()));
            assert_eq!(map.layout(second), Some(Layout::Free));
        }
    }

    mod scan_argv {
        use super::*;

        fn argv(items: &[&str]) -> Vec<String> {
            items.iter().map(|s| s.to_string()).collect()
        }

        #[test]
        fn every_flag_and_bare_exclude_is_read() {
            let args = crate::scan::parse_args(&argv(&[
                "/src",
                "--module",
                "sub",
                "--id-prefix",
                "g1",
                "--targets",
                "t.txt",
                "--cache-dir",
                "c",
                "--cache-key",
                "k",
                "--copy-path",
                "cp",
                "-std=ibm",
                "sub/x.cob",
            ]))
            .unwrap();
            assert_eq!(args.root, "/src");
            assert_eq!(args.modules, ["sub"]);
            assert_eq!(args.id_prefix, "g1");
            assert_eq!(args.targets.as_deref(), Some("t.txt"));
            assert_eq!(args.copy_paths, ["cp"]);
            assert_eq!(args.dialect, Dialect::Ibm);
            assert_eq!(args.excludes, ["sub/x.cob"]);
        }

        #[test]
        fn defaults_without_flags() {
            let args = crate::scan::parse_args(&argv(&["/src"])).unwrap();
            assert_eq!(args.id_prefix, "n");
            assert_eq!(args.dialect, Dialect::DEFAULT);
            assert!(args.targets.is_none() && args.modules.is_empty());
        }

        #[test]
        fn value_flag_without_its_value_is_an_exclude() {
            let args = crate::scan::parse_args(&argv(&["/src", "--module"])).unwrap();
            assert!(args.modules.is_empty());
            assert_eq!(args.excludes, ["--module"]);
        }

        #[test]
        fn missing_directory_and_unknown_dialect_are_refused() {
            assert!(crate::scan::parse_args(&[]).is_err());
            let err = crate::scan::parse_args(&argv(&["/src", "-std=cobol2023"])).unwrap_err();
            assert!(err.contains("placeholder"), "{err}");
        }
    }
}

fn search() -> crate::preprocess::Search {
    crate::preprocess::Search {
        scan_root: "/root".into(),
        copy_paths: Vec::new(),
    }
}

/// Expands `text` (named `name`) against the in-memory `files`, then parses it; keeps the map
/// so origins can be named. The same stages as `parse_source`, without discarding the map.
fn expand(
    name: &str,
    text: &str,
    files: &[(&str, &str)],
) -> (
    crate::ast::CompilationUnit,
    Vec<crate::diag::Diagnostic>,
    SourceMap,
) {
    let store: std::collections::HashMap<std::path::PathBuf, String> = files
        .iter()
        .map(|(p, t)| (std::path::PathBuf::from(p), t.to_string()))
        .collect();
    let lookup = |path: &std::path::Path| store.get(path).cloned();
    let mut map = SourceMap::default();
    let (tokens, mut diags) =
        crate::preprocess::preprocess(&mut map, name, text, Dialect::Cobol85, &search(), &lookup);
    let (unit, more) = crate::parser::parse_compilation_unit(&map, Dialect::Cobol85, &tokens);
    diags.extend(more);
    (unit, diags, map)
}

mod int {
    use super::*;

    /// A fixed-format IDENTIFICATION, DATA and PROCEDURE header around `data` and `proc`.
    fn program(data: &[String], proc: &[String]) -> String {
        let mut lines = vec![
            at(8, "IDENTIFICATION DIVISION."),
            at(8, "PROGRAM-ID. PROG."),
            at(8, "DATA DIVISION."),
            at(8, "WORKING-STORAGE SECTION."),
        ];
        lines.extend(data.iter().cloned());
        lines.push(at(8, "PROCEDURE DIVISION."));
        lines.extend(proc.iter().cloned());
        lines.join("\n")
    }

    #[test]
    fn copybook_declaration_maps_back_to_its_copybook_line() {
        let copy = [at(8, "01 REC-CB."), at(12, "05 F-CB PIC X(4).")].join("\n");
        let main = program(
            &[at(12, "COPY REC.")],
            &[at(8, "MAIN-PARA."), at(12, "DISPLAY F-CB.")],
        );
        let (unit, diags, map) = expand("/root/prog.cob", &main, &[("/root/REC.cpy", &copy)]);
        assert!(diags.is_empty(), "{diags:?}");
        let rec = &unit.programs[0].data.as_ref().unwrap().sections[0].entries[0];
        let name = rec.name.as_ref().unwrap();
        assert_eq!(
            (
                name.text.as_str(),
                map.name(name.span.start.file),
                name.span.start.line,
                name.span.start.col
            ),
            ("REC-CB", "/root/REC.cpy", 1, 11)
        );
        let child = rec.children[0].name.as_ref().unwrap();
        assert_eq!(
            (
                map.name(child.span.start.file),
                child.span.start.line,
                child.span.start.col
            ),
            ("/root/REC.cpy", 2, 15)
        );
    }

    #[test]
    fn each_file_records_the_layout_it_was_read_through() {
        let copy = ">>SOURCE FREE\n01 REC-CB PIC X.\n";
        let main = program(
            &[at(12, "COPY REC.")],
            &[at(8, "MAIN-PARA."), at(12, "STOP RUN.")],
        );
        let lookup = |p: &std::path::Path| {
            (p == std::path::Path::new("/root/REC.cpy")).then(|| copy.to_string())
        };
        let mut map = SourceMap::default();
        let (tokens, _) = crate::preprocess::preprocess(
            &mut map,
            "/root/prog.cob",
            &main,
            Dialect::Cobol85,
            &search(),
            &lookup,
        );
        let file_named = |name: &str| {
            tokens
                .iter()
                .map(|m| m.origin.file)
                .find(|f| map.name(*f) == name)
                .unwrap()
        };
        let main_id = file_named("/root/prog.cob");
        let copy_id = file_named("/root/REC.cpy");
        assert_eq!(map.layout(main_id), Some(fixed()));
        assert_eq!(map.layout(copy_id), Some(crate::format::Layout::Free));
    }

    #[test]
    fn replace_in_the_main_file_applies_across_copy() {
        let copy = [at(8, "01 REC-CB."), at(12, "05 F-CB PIC TAG.")].join("\n");
        let main = program(
            &[at(12, "REPLACE ==TAG== BY ==9==."), at(12, "COPY REC.")],
            &[at(8, "MAIN-PARA."), at(12, "STOP RUN.")],
        );
        let (unit, diags, _) = expand("/root/prog.cob", &main, &[("/root/REC.cpy", &copy)]);
        assert!(diags.is_empty(), "{diags:?}");
        let rec = &unit.programs[0].data.as_ref().unwrap().sections[0].entries[0];
        assert_eq!(
            rec.children[0].pic.as_ref().map(|p| p.text.as_str()),
            Some("9")
        );
    }

    #[test]
    fn a_syntax_error_costs_only_its_sentence() {
        let text = [
            at(8, "IDENTIFICATION DIVISION."),
            at(8, "PROGRAM-ID. PROG."),
            at(8, "PROCEDURE DIVISION."),
            at(8, "MAIN-PARA."),
            at(12, "DISPLAY 'A'."),
            at(8, "BAD-PARA."),
            at(12, "FROB X."),
            at(8, "LATER-PARA."),
            at(12, "STOP RUN."),
        ]
        .join("\n");
        let (unit, diags) =
            crate::parser::parse_source("prog.cob", &text, Dialect::Cobol85, &search(), &|_| None);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].line, 7, "{diags:?}");
        let paragraphs = &unit.programs[0].procedure.as_ref().unwrap().paragraphs;
        let names: Vec<&str> = paragraphs.iter().map(|p| p.name.text.as_str()).collect();
        assert_eq!(names, ["MAIN-PARA", "BAD-PARA", "LATER-PARA"]);
        let counts: Vec<usize> = paragraphs
            .iter()
            .map(|p| p.sentences.iter().map(|s| s.statements.len()).sum())
            .collect();
        assert_eq!(counts, [1, 0, 1]);
    }

    #[test]
    fn header_error_skips_one_program_and_keeps_the_next() {
        let (unit, diags) = parse_file(
            "IDENTIFICATION DIVISION.\nAUTHOR. X.\nPROCEDURE DIVISION.\nSTOP RUN.\nIDENTIFICATION DIVISION.\nPROGRAM-ID. TWO.\nPROCEDURE DIVISION.\nMAIN.\nSTOP RUN.\n",
        );
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(unit.programs.len(), 1);
        assert_eq!(unit.programs[0].name.text, "TWO");
    }

    /// The paragraph names of the one program in `unit`.
    fn paragraph_names(unit: &crate::ast::CompilationUnit) -> Vec<&str> {
        let procedure = unit.programs[0].procedure.as_ref().unwrap();
        procedure
            .paragraphs
            .iter()
            .map(|p| p.name.text.as_str())
            .collect()
    }

    #[test]
    fn fixed_paragraph_header_ends_a_sentence_missing_its_period() {
        let text = [
            at(8, "IDENTIFICATION DIVISION."),
            at(8, "PROGRAM-ID. PROG."),
            at(8, "PROCEDURE DIVISION."),
            at(8, "MAIN-PARA."),
            at(12, "DISPLAY X"),
            at(8, "NEXT-PARA."),
            at(12, "STOP RUN."),
        ]
        .join("\n");
        let (unit, diags) =
            crate::parser::parse_source("prog.cob", &text, Dialect::Cobol85, &search(), &|_| None);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert!(
            diags[0].message.contains("not ended by a period"),
            "{diags:?}"
        );
        assert_eq!(diags[0].line, 5, "{diags:?}");
        assert_eq!(paragraph_names(&unit), ["MAIN-PARA", "NEXT-PARA"]);
        let procedure = unit.programs[0].procedure.as_ref().unwrap();
        assert_eq!(procedure.paragraphs[1].sentences[0].statements.len(), 1);
    }

    #[test]
    fn fixed_section_header_ends_a_sentence_missing_its_period() {
        let text = [
            at(8, "IDENTIFICATION DIVISION."),
            at(8, "PROGRAM-ID. PROG."),
            at(8, "PROCEDURE DIVISION."),
            at(8, "MAIN-PARA."),
            at(12, "DISPLAY X"),
            at(8, "SEC-A SECTION."),
            at(8, "PARA-B."),
            at(12, "STOP RUN."),
        ]
        .join("\n");
        let (unit, diags) =
            crate::parser::parse_source("prog.cob", &text, Dialect::Cobol85, &search(), &|_| None);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert!(
            diags[0].message.contains("not ended by a period"),
            "{diags:?}"
        );
        let procedure = unit.programs[0].procedure.as_ref().unwrap();
        let sections: Vec<&str> = procedure
            .sections
            .iter()
            .map(|s| s.name.text.as_str())
            .collect();
        assert_eq!(sections, ["SEC-A"]);
    }

    #[test]
    fn free_section_header_ends_a_sentence_missing_its_period() {
        let (unit, diags) =
            parse_file("IDENTIFICATION DIVISION.\nPROGRAM-ID. PROG.\nPROCEDURE DIVISION.\nMAIN-PARA.\nDISPLAY X\nSEC-A SECTION.\nPARA-B.\nSTOP RUN.\n");
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert!(
            diags[0].message.contains("not ended by a period"),
            "{diags:?}"
        );
        let procedure = unit.programs[0].procedure.as_ref().unwrap();
        let sections: Vec<&str> = procedure
            .sections
            .iter()
            .map(|s| s.name.text.as_str())
            .collect();
        assert_eq!(sections, ["SEC-A"]);
    }

    #[test]
    fn free_paragraph_name_does_not_end_a_sentence() {
        let (unit, diags) = parse_file(
            "IDENTIFICATION DIVISION.\nPROGRAM-ID. PROG.\nPROCEDURE DIVISION.\nMAIN-PARA.\nMOVE A TO\nB.\nSTOP RUN.\n",
        );
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(paragraph_names(&unit), ["MAIN-PARA"]);
    }

    #[test]
    fn fixed_name_in_area_b_does_not_end_a_sentence() {
        let text = [
            at(8, "IDENTIFICATION DIVISION."),
            at(8, "PROGRAM-ID. PROG."),
            at(8, "PROCEDURE DIVISION."),
            at(8, "MAIN-PARA."),
            at(12, "MOVE A TO"),
            at(12, "B."),
        ]
        .join("\n");
        let (unit, diags) =
            crate::parser::parse_source("prog.cob", &text, Dialect::Cobol85, &search(), &|_| None);
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(paragraph_names(&unit), ["MAIN-PARA"]);
    }

    #[test]
    fn replaced_token_at_the_previous_position_never_starts_a_header() {
        let text = [
            at(8, "IDENTIFICATION DIVISION."),
            at(8, "PROGRAM-ID. PROG."),
            at(8, "PROCEDURE DIVISION."),
            at(8, "MAIN-PARA."),
            at(12, "REPLACE ==Q== BY ==A B==."),
            at(8, "Q."),
        ]
        .join("\n");
        let (unit, diags) =
            crate::parser::parse_source("prog.cob", &text, Dialect::Cobol85, &search(), &|_| None);
        assert!(
            diags
                .iter()
                .all(|d| !d.message.contains("not ended by a period")),
            "{diags:?}"
        );
        assert_eq!(paragraph_names(&unit), ["MAIN-PARA"]);
    }

    #[test]
    fn text_before_the_first_program_is_one_diagnostic() {
        let (unit, diags) = parse_file(
            "JUNK X.\nIDENTIFICATION DIVISION.\nPROGRAM-ID. P.\nPROCEDURE DIVISION.\nMAIN.\nSTOP RUN.\n",
        );
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(unit.programs.len(), 1);
    }

    #[test]
    fn area_a_departure_is_located_in_the_original_file() {
        let text = [
            at(8, "IDENTIFICATION DIVISION."),
            at(8, "PROGRAM-ID. PROG."),
            at(8, "PROCEDURE DIVISION."),
            at(8, "MAIN-PARA."),
            at(8, "DISPLAY X."),
        ]
        .join("\n");
        let (_, diags) =
            crate::parser::parse_source("prog.cob", &text, Dialect::Cobol85, &search(), &|_| None);
        assert!(
            diags.iter().any(|d| {
                (d.file.as_str(), d.line, d.col) == ("prog.cob", 5, 8)
                    && d.message.contains("area A")
            }),
            "{diags:?}"
        );
    }

    #[test]
    fn word_over_the_limit_is_located_and_lifted_by_the_later_dialect() {
        let text = [
            at(8, "IDENTIFICATION DIVISION."),
            at(8, "PROGRAM-ID. PROG."),
            at(8, "PROCEDURE DIVISION."),
            at(8, "MAIN-PARA."),
            at(12, &format!("DISPLAY {}.", "A".repeat(31))),
        ]
        .join("\n");
        let (_, diags) =
            crate::parser::parse_source("prog.cob", &text, Dialect::Cobol85, &search(), &|_| None);
        assert!(
            diags
                .iter()
                .any(|d| (d.file.as_str(), d.line, d.col) == ("prog.cob", 5, 20)),
            "{diags:?}"
        );
        let (_, diags) =
            crate::parser::parse_source("prog.cob", &text, Dialect::Cobol2002, &search(), &|_| {
                None
            });
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn oo_word_is_an_ordinary_data_name_under_cobol85_but_class_is_reserved() {
        let (unit, diags) = parse_file(
            "IDENTIFICATION DIVISION.\nPROGRAM-ID. P.\nDATA DIVISION.\nWORKING-STORAGE SECTION.\n01 OBJECT PIC X.\n01 INVOKE PIC X.\nPROCEDURE DIVISION.\nMAIN.\nSTOP RUN.\n",
        );
        assert!(diags.is_empty(), "{diags:?}");
        let entries = &unit.programs[0].data.as_ref().unwrap().sections[0].entries;
        assert_eq!(names(entries), ["OBJECT", "INVOKE"]);
        let (unit, diags) = parse_file(
            "IDENTIFICATION DIVISION.\nPROGRAM-ID. P.\nDATA DIVISION.\nWORKING-STORAGE SECTION.\n01 CLASS PIC X.\nPROCEDURE DIVISION.\nMAIN.\nSTOP RUN.\n",
        );
        assert_eq!(diags.len(), 1, "{diags:?}");
        let entries = &unit.programs[0].data.as_ref().unwrap().sections[0].entries;
        assert!(!names(entries).contains(&"CLASS"), "{entries:?}");
    }

    #[test]
    fn slash_comment_and_zzz_picture_in_a_real_program() {
        let text = [
            at(8, "IDENTIFICATION DIVISION."),
            at(8, "PROGRAM-ID. PROG."),
            at(8, "DATA DIVISION."),
            at(8, "WORKING-STORAGE SECTION."),
            at(8, "01 REC."),
            at(12, "05 E PIC ZZZ.ZZ."),
            at(7, "/ PAGE EJECT"),
            at(8, "PROCEDURE DIVISION."),
            at(8, "MAIN-PARA."),
            at(12, "STOP RUN."),
        ]
        .join("\n");
        let (unit, diags) =
            crate::parser::parse_source("prog.cob", &text, Dialect::Cobol85, &search(), &|_| None);
        assert!(diags.is_empty(), "{diags:?}");
        let rec = &unit.programs[0].data.as_ref().unwrap().sections[0].entries[0];
        assert_eq!(
            rec.children[0].pic.as_ref().map(|p| p.text.as_str()),
            Some("ZZZ.ZZ")
        );
        let paragraphs = &unit.programs[0].procedure.as_ref().unwrap().paragraphs;
        assert_eq!(paragraphs[0].name.text, "MAIN-PARA");
    }

    use crate::resolve::{ProcKind, Resolution};

    /// Sections, unsectioned paragraphs and PERFORMs; the procs are U-PARA, S-ONE{P-ONE,P-TWO}, S-TWO{P-THREE}.
    const SECTIONED: &str = "IDENTIFICATION DIVISION.\nPROGRAM-ID. PROG.\nPROCEDURE DIVISION.\nU-PARA.\n    PERFORM S-TWO.\n    PERFORM P-ONE THRU P-THREE.\n    PERFORM S-ONE THRU S-TWO.\n    PERFORM P-TWO OF S-ONE.\n    PERFORM NOPE.\nS-ONE SECTION.\nP-ONE.\n    DISPLAY X.\nP-TWO.\n    DISPLAY X.\nS-TWO SECTION.\nP-THREE.\n    DISPLAY X.\n";

    /// The PERFORM ranges of a program's procedure division, in source order.
    fn performs(program: &crate::ast::Program) -> Vec<crate::ast::ProcRange> {
        let Some(div) = &program.procedure else {
            return Vec::new();
        };
        let declaratives = div.declaratives.iter().flat_map(|s| s.paragraphs.iter());
        let sectioned = div.sections.iter().flat_map(|s| s.paragraphs.iter());
        declaratives
            .chain(div.paragraphs.iter())
            .chain(sectioned)
            .flat_map(|p| p.sentences.iter().flat_map(|s| s.statements.iter()))
            .filter_map(|st| match &st.transfer {
                Some(Transfer::Perform(Some(range))) => Some(range.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn section_paragraph_index_owns_paragraphs_in_source_order() {
        let (unit, diags) = parse_file(SECTIONED);
        assert!(diags.is_empty(), "{diags:?}");
        let scopes = crate::resolve::scopes(&unit);
        let procs = &scopes[0].procs;
        let index: Vec<(&str, ProcKind, Option<usize>)> = procs
            .iter()
            .map(|p| (p.name.as_str(), p.kind, p.section))
            .collect();
        assert_eq!(
            index,
            [
                ("U-PARA", ProcKind::Paragraph, None),
                ("S-ONE", ProcKind::Section, None),
                ("P-ONE", ProcKind::Paragraph, Some(1)),
                ("P-TWO", ProcKind::Paragraph, Some(1)),
                ("S-TWO", ProcKind::Section, None),
                ("P-THREE", ProcKind::Paragraph, Some(4)),
            ]
        );
        assert_eq!((procs[1].last, procs[4].last), (3, 5));
    }

    #[test]
    fn perform_resolves_one_paragraph_or_section_and_qualified_names() {
        let (unit, _) = parse_file(SECTIONED);
        let scope = &crate::resolve::scopes(&unit)[0];
        let targets: Vec<Resolution> = performs(&unit.programs[0])
            .iter()
            .map(|r| scope.resolve(&r.from))
            .collect();
        assert_eq!(
            targets,
            [
                Resolution::One(4),
                Resolution::One(2),
                Resolution::One(1),
                Resolution::One(3),
                Resolution::Undefined,
            ]
        );
    }

    #[test]
    fn perform_thru_spans_paragraphs_across_section_boundaries() {
        let (unit, _) = parse_file(SECTIONED);
        let scope = &crate::resolve::scopes(&unit)[0];
        let ranges = performs(&unit.programs[0]);
        let thru = |i: usize| scope.range(&ranges[i].from, ranges[i].thru.as_ref().unwrap());
        // P-ONE THRU P-THREE crosses S-ONE into S-TWO; S-ONE THRU S-TWO covers both sections' paragraphs.
        assert_eq!(thru(1), Resolution::Range(vec![2, 3, 5]));
        assert_eq!(thru(2), Resolution::Range(vec![2, 3, 5]));
    }

    #[test]
    fn duplicate_unqualified_name_is_ambiguous_and_qualifying_picks_one() {
        let (unit, diags) = parse_file(
            "IDENTIFICATION DIVISION.\nPROGRAM-ID. DUP.\nPROCEDURE DIVISION.\nU-PARA.\n    PERFORM SAME.\n    PERFORM SAME OF S-TWO.\nS-ONE SECTION.\nSAME.\n    DISPLAY X.\nS-TWO SECTION.\nSAME.\n    DISPLAY X.\n",
        );
        assert!(diags.is_empty(), "{diags:?}");
        let scope = &crate::resolve::scopes(&unit)[0];
        let ranges = performs(&unit.programs[0]);
        assert_eq!(scope.resolve(&ranges[0].from), Resolution::Ambiguous);
        assert_eq!(scope.resolve(&ranges[1].from), Resolution::One(4));
    }

    #[test]
    fn nested_program_names_stay_in_their_own_scope() {
        let (unit, diags) = parse_file(
            "IDENTIFICATION DIVISION.\nPROGRAM-ID. OUTER.\nPROCEDURE DIVISION.\nO-PARA.\n    PERFORM IN-PARA.\nIDENTIFICATION DIVISION.\nPROGRAM-ID. INNER.\nPROCEDURE DIVISION.\nIN-PARA.\n    PERFORM O-PARA.\nEND PROGRAM INNER.\nEND PROGRAM OUTER.\n",
        );
        assert!(diags.is_empty(), "{diags:?}");
        let scopes = crate::resolve::scopes(&unit);
        assert_eq!(scopes.len(), 2);
        assert_eq!(
            (scopes[1].name.as_str(), scopes[1].parent),
            ("INNER", Some(0))
        );
        let outer_call = &performs(&unit.programs[0])[0].from;
        assert_eq!(scopes[0].resolve(outer_call), Resolution::Undefined);
        let inner_call = &performs(&unit.programs[0].nested[0])[0].from;
        assert_eq!(scopes[1].resolve(inner_call), Resolution::Undefined);
        assert_eq!(scopes[1].procs[0].name, "IN-PARA");
    }

    /// REC (its second declaration `rec` is dropped), OTHER, LK in LINKAGE; 77 W is no record.
    const RECORDS: &str = "IDENTIFICATION DIVISION.\nPROGRAM-ID. PROG.\nDATA DIVISION.\nWORKING-STORAGE SECTION.\n01 REC.\n    05 F PIC X.\n01 rec.\n    05 G PIC X.\n01 OTHER.\n    05 F PIC X.\n77 W PIC X.\nLINKAGE SECTION.\n01 LK.\n    05 F2 PIC X.\n";

    #[test]
    fn records_are_level_01_entries_and_the_first_declaration_wins() {
        let (unit, diags) = parse_file(RECORDS);
        assert!(diags.is_empty(), "{diags:?}");
        let scope = &crate::resolve::scopes(&unit)[0];
        assert_eq!(names(&scope.records), ["REC", "OTHER", "LK"]);
        assert_eq!(names(&scope.records[0].children), ["F"]);
    }

    #[test]
    fn data_names_resolve_through_their_record_or_not_at_all() {
        let (unit, diags) = parse_file(RECORDS);
        assert!(diags.is_empty(), "{diags:?}");
        let scope = &crate::resolve::scopes(&unit)[0];
        let span = unit.programs[0].name.span;
        let word = |text: &str| crate::ast::Name {
            text: text.to_string(),
            span,
        };
        let qn = |name: &str, quals: &[&str]| crate::ast::QualifiedName {
            name: word(name),
            qualifiers: quals.iter().map(|q| word(q)).collect(),
        };
        let cases: [(crate::ast::QualifiedName, Option<usize>); 10] = [
            (qn("REC", &[]), Some(0)),
            (qn("LK", &[]), Some(2)),
            (qn("F2", &[]), Some(2)),
            (qn("F", &["OTHER"]), Some(1)),
            (qn("F", &["REC"]), Some(0)),
            (qn("G", &[]), None),
            (qn("F", &[]), None),
            (qn("W", &[]), None),
            (qn("F", &["NOPE"]), None),
            (qn("F", &["OTHER", "REC"]), None),
        ];
        for (name, want) in cases {
            assert_eq!(scope.data(&name), want, "{name:?}");
        }
    }

    /// Every CALL target of `statements`, nested branches included, in source order.
    fn call_targets(statements: &[crate::ast::Statement], out: &mut Vec<CallTarget>) {
        for statement in statements {
            if let Some(Transfer::Call(target)) = &statement.transfer {
                out.push(target.clone());
            }
            call_targets(&statement.children, out);
        }
    }

    /// Parses free-format `text` and resolves each CALL of its programs, in source order,
    /// against all of them.
    fn resolve_calls(
        text: &str,
    ) -> Vec<(
        crate::resolve::CallResolution,
        Option<crate::diag::Diagnostic>,
    )> {
        let (unit, _, map) = expand("prog.cob", &format!(">>SOURCE FREE\n{text}"), &[]);
        let scopes = crate::resolve::scopes(&unit);
        let mut targets = Vec::new();
        for program in &unit.programs {
            if let Some(div) = &program.procedure {
                for paragraph in div
                    .paragraphs
                    .iter()
                    .chain(div.sections.iter().flat_map(|s| &s.paragraphs))
                {
                    for sentence in &paragraph.sentences {
                        call_targets(&sentence.statements, &mut targets);
                    }
                }
            }
        }
        targets
            .iter()
            .map(|t| crate::resolve::resolve_call(&scopes, &map, t))
            .collect()
    }

    /// `OUTER` makes the CALLs in `calls`; `SUB-ONE` is a second program beside it.
    fn caller(calls: &str) -> String {
        format!(
            "IDENTIFICATION DIVISION.\nPROGRAM-ID. OUTER.\nPROCEDURE DIVISION.\nMAIN-PARA.\n    {calls}\nEND PROGRAM OUTER.\nIDENTIFICATION DIVISION.\nPROGRAM-ID. SUB-ONE.\nPROCEDURE DIVISION.\nSUB-PARA.\n    STOP RUN.\n"
        )
    }

    #[test]
    fn literal_call_matching_a_program_id_exactly_has_no_warning() {
        let got = resolve_calls(&caller("CALL 'SUB-ONE'."));
        assert_eq!(got, [(crate::resolve::CallResolution::Program(1), None)]);
    }

    #[test]
    fn literal_call_matching_case_insensitively_warns_at_the_literal() {
        let got = resolve_calls(&caller("CALL 'sub-one'."));
        let (resolution, diag) = &got[0];
        assert_eq!(*resolution, crate::resolve::CallResolution::Program(1));
        let diag = diag.as_ref().expect("a case-insensitive match warns");
        assert_eq!(diag.file, "prog.cob");
        assert!(diag.message.contains("case-insensitively"), "{diag:?}");
    }

    #[test]
    fn literal_naming_no_program_is_external_and_data_name_is_dynamic() {
        let got = resolve_calls(&caller("CALL 'NOPE'.\n    CALL P-NAME."));
        assert_eq!(
            got,
            [
                (crate::resolve::CallResolution::External, None),
                (crate::resolve::CallResolution::Dynamic, None),
            ]
        );
    }

    #[test]
    fn duplicate_program_id_is_ambiguous_not_a_guess() {
        let text = format!(
            "{}IDENTIFICATION DIVISION.\nPROGRAM-ID. SUB-ONE.\nPROCEDURE DIVISION.\nSUB-PARA.\n    STOP RUN.\n",
            caller("CALL 'SUB-ONE'.")
        );
        let got = resolve_calls(&text);
        assert_eq!(got[0].0, crate::resolve::CallResolution::Ambiguous);
    }

    /// Scans `text` as `path` against the in-memory `files`; returns the JSONL records as
    /// values and the diagnostics.
    fn scan_text(
        path: &str,
        text: &str,
        files: &[(&str, &str)],
    ) -> (Vec<serde_json::Value>, Vec<crate::diag::Diagnostic>) {
        let store: std::collections::HashMap<std::path::PathBuf, String> = files
            .iter()
            .map(|(p, t)| (std::path::PathBuf::from(p), t.to_string()))
            .collect();
        let lookup = |p: &std::path::Path| store.get(p).cloned();
        let mut emitter = crate::scan::Emitter::new(std::path::Path::new("/root"), "n");
        let diags = emitter.scan_file(path, text, Dialect::Cobol85, &search(), &lookup);
        (jsonl(&emitter), diags)
    }

    /// Scans every `(path, text)` of `tree` into one emitter under the `/root` base, the
    /// copybook lookup reading the same tree.
    fn scan_tree(tree: &[(&str, &str)]) -> crate::scan::Emitter {
        let store: std::collections::HashMap<std::path::PathBuf, String> = tree
            .iter()
            .map(|(p, t)| (std::path::PathBuf::from(p), t.to_string()))
            .collect();
        let lookup = |p: &std::path::Path| store.get(p).cloned();
        let mut emitter = crate::scan::Emitter::new(std::path::Path::new("/root"), "n");
        for (path, text) in tree {
            emitter.scan_file(path, text, Dialect::Cobol85, &search(), &lookup);
        }
        emitter
    }

    /// The emitter's records as JSON values, one per JSONL line.
    fn jsonl(emitter: &crate::scan::Emitter) -> Vec<serde_json::Value> {
        let mut out = Vec::new();
        emitter.write_jsonl(&mut out).unwrap();
        String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    /// A minimal program `name` (PROCEDURE with one paragraph) as a standalone source.
    fn prog(name: &str) -> String {
        [
            at(8, "IDENTIFICATION DIVISION."),
            at(8, &format!("PROGRAM-ID. {name}.")),
            at(8, "PROCEDURE DIVISION."),
            at(8, "MAIN-PARA."),
            at(12, "STOP RUN."),
        ]
        .join("\n")
    }

    #[test]
    fn scan_emits_module_file_program_paragraph_and_contains_lines() {
        let text = [
            at(8, "IDENTIFICATION DIVISION."),
            at(8, "PROGRAM-ID. PROG."),
            at(8, "PROCEDURE DIVISION."),
            at(8, "MAIN-PARA."),
            at(12, "STOP RUN."),
        ]
        .join("\n");
        let (recs, diags) = scan_text("/root/prog.cob", &text, &[]);
        assert!(diags.is_empty(), "{diags:?}");
        let len = text.len();
        let para = text.find("MAIN-PARA").unwrap();
        assert_eq!(
            recs,
            [
                serde_json::json!({"type":"module","fqn":""}),
                serde_json::json!({"type":"file","path":"/root/prog.cob","parent":"","start_line":1,"end_line":5}),
                serde_json::json!({"type":"struct","id":"n1","parent":"","name":"PROG","path":"/root/prog.cob","start":7,"end":len,"start_line":1,"end_line":5}),
                serde_json::json!({"type":"function","id":"n2","parent":"PROG","name":"MAIN-PARA","file":"/root/prog.cob","path":"/root/prog.cob","start":para,"end":len,"start_line":4,"end_line":5}),
                serde_json::json!({"type":"contains","from":"n1","to":"n2"}),
            ]
        );
    }

    #[test]
    fn copybook_record_offsets_index_the_copybook_text() {
        let copy = [at(8, "01 REC-CB."), at(12, "05 F-CB PIC X.")].join("\n");
        let main = [
            at(8, "IDENTIFICATION DIVISION."),
            at(8, "PROGRAM-ID. PROG."),
            at(8, "DATA DIVISION."),
            at(8, "WORKING-STORAGE SECTION."),
            at(12, "COPY REC."),
            at(8, "PROCEDURE DIVISION."),
            at(8, "MAIN-PARA."),
            at(12, "STOP RUN."),
        ]
        .join("\n");
        let (recs, diags) = scan_text("/root/prog.cob", &main, &[("/root/REC.cpy", &copy)]);
        assert!(diags.is_empty(), "{diags:?}");
        let rec = recs
            .iter()
            .find(|r| r["type"] == "struct" && r["name"] == "REC-CB")
            .expect("REC-CB struct");
        assert_eq!(rec["path"], "/root/REC.cpy");
        assert_eq!(rec["parent"], "PROG");
        assert_eq!(
            (rec["start_line"].as_u64(), rec["end_line"].as_u64()),
            (Some(1), Some(1))
        );
        let (start, end) = (rec["start"].as_u64().unwrap(), rec["end"].as_u64().unwrap());
        assert_eq!(&copy[start as usize..end as usize], "01 REC-CB.");
        assert!(recs
            .iter()
            .any(|r| r["type"] == "contains" && r["from"] == "n1" && r["to"] == rec["id"]));
    }

    #[test]
    fn non_ascii_before_a_column_is_counted_in_characters() {
        let text = [
            at(8, "IDENTIFICATION DIVISION."),
            at(8, "PROGRAM-ID. PROG."),
            at(8, "DATA DIVISION."),
            at(8, "WORKING-STORAGE SECTION."),
            at(8, "01 F-E PIC X VALUE \"é\". 01 REC-C PIC X."),
            at(8, "PROCEDURE DIVISION."),
            at(8, "MAIN-PARA."),
            at(12, "STOP RUN."),
        ]
        .join("\n");
        let (recs, diags) = scan_text("/root/prog.cob", &text, &[]);
        assert!(diags.is_empty(), "{diags:?}");
        let rec = recs
            .iter()
            .find(|r| r["type"] == "struct" && r["name"] == "REC-C")
            .expect("REC-C struct");
        let start = rec["start"].as_u64().unwrap() as usize;
        assert_eq!(start, text.find("01 REC-C").unwrap());
        assert!(text[..start].chars().count() < start, "é is two bytes");
        assert_eq!(&text[start..start + 8], "01 REC-C");
    }

    #[test]
    fn module_is_relative_to_the_scan_base_and_empty_at_it() {
        let text = prog("PROG");
        let (recs, _) = scan_text("/root/src/cobol/prog.cob", &text, &[]);
        assert_eq!(
            recs[0],
            serde_json::json!({"type":"module","fqn":"src/cobol"})
        );
        assert_eq!(recs[1]["parent"], "src/cobol");
        assert_eq!(recs[1]["path"], "/root/src/cobol/prog.cob");
        let (recs, _) = scan_text("/root/prog.cob", &text, &[]);
        assert_eq!(recs[0], serde_json::json!({"type":"module","fqn":""}));
    }

    #[test]
    fn ids_take_the_id_prefix() {
        let mut emitter = crate::scan::Emitter::new(std::path::Path::new("/root"), "g1");
        let none = |_: &std::path::Path| None::<String>;
        emitter.scan_file(
            "/root/prog.cob",
            &prog("PROG"),
            Dialect::Cobol85,
            &search(),
            &none,
        );
        let recs = jsonl(&emitter);
        assert_eq!(recs[2]["id"], "g11");
        assert_eq!(recs[3]["id"], "g12");
        assert_eq!(
            recs[4],
            serde_json::json!({"type":"contains","from":"g11","to":"g12"})
        );
    }

    #[test]
    fn targets_emit_only_the_target_files_facts_and_resolution_is_unchanged() {
        let copy = [at(8, "01 REC-CB."), at(12, "05 F-CB PIC X.")].join("\n");
        let main = [
            at(8, "IDENTIFICATION DIVISION."),
            at(8, "PROGRAM-ID. PROG."),
            at(8, "DATA DIVISION."),
            at(8, "WORKING-STORAGE SECTION."),
            at(12, "COPY REC."),
            at(8, "PROCEDURE DIVISION."),
            at(8, "MAIN-PARA."),
            at(12, "STOP RUN."),
        ]
        .join("\n");
        let other = prog("OTHER");
        let tree = [
            ("/root/prog.cob", main.as_str()),
            ("/root/REC.cpy", copy.as_str()),
            ("/root/other.cob", other.as_str()),
        ];
        let unfiltered = jsonl(&scan_tree(&tree));
        let target: std::collections::HashSet<String> =
            ["/root/REC.cpy".to_string()].into_iter().collect();
        let mut filtered = scan_tree(&tree);
        filtered.retain_targets(&target);
        let recs = jsonl(&filtered);

        // The copybook's File and its declaration (scoped to the main program, whose
        // own records are dropped); the module stays because the copybook sits in it.
        assert_eq!(recs.len(), 3, "{recs:?}");
        assert!(recs.contains(&serde_json::json!({"type":"module","fqn":""})));
        assert!(recs
            .iter()
            .any(|r| r["type"] == "file" && r["path"] == "/root/REC.cpy"));
        let rec = recs
            .iter()
            .find(|r| r["type"] == "struct" && r["name"] == "REC-CB")
            .expect("REC-CB struct");
        let same = unfiltered
            .iter()
            .find(|r| r["type"] == "struct" && r["name"] == "REC-CB")
            .expect("REC-CB struct unfiltered");
        assert_eq!(rec, same);
    }

    /// The id of the first record named `name`.
    fn id_of(recs: &[serde_json::Value], name: &str) -> String {
        recs.iter()
            .find(|r| r["name"] == name)
            .and_then(|r| r["id"].as_str())
            .expect("named record")
            .to_string()
    }

    /// Records of the given `types`, sorted so the comparison ignores emission order.
    fn edges(recs: &[serde_json::Value], types: &[&str]) -> Vec<serde_json::Value> {
        let mut out: Vec<serde_json::Value> = recs
            .iter()
            .filter(|r| types.contains(&r["type"].as_str().unwrap_or("")))
            .cloned()
            .collect();
        out.sort_by_key(|v| v.to_string());
        out
    }

    /// MAIN-PARA performs P-ONE, then P-ONE THRU P-THREE, and GOes TO P-TWO; P-THREE (in
    /// section S-ONE) performs the section.
    fn control() -> String {
        [
            at(8, "IDENTIFICATION DIVISION."),
            at(8, "PROGRAM-ID. PROG."),
            at(8, "PROCEDURE DIVISION."),
            at(8, "MAIN-PARA."),
            at(12, "PERFORM P-ONE."),
            at(12, "PERFORM P-ONE THRU P-THREE."),
            at(12, "GO TO P-TWO."),
            at(8, "P-ONE."),
            at(12, "DISPLAY X."),
            at(8, "P-TWO."),
            at(12, "DISPLAY X."),
            at(8, "S-ONE SECTION."),
            at(8, "P-THREE."),
            at(12, "PERFORM S-ONE."),
        ]
        .join("\n")
    }

    #[test]
    fn perform_and_goto_paragraphs_are_calls_with_their_kind_and_sections_are_uses() {
        let (recs, diags) = scan_text("/root/prog.cob", &control(), &[]);
        assert!(diags.is_empty(), "{diags:?}");
        let main = id_of(&recs, "MAIN-PARA");
        let (p1, p2, p3) = (
            id_of(&recs, "P-ONE"),
            id_of(&recs, "P-TWO"),
            id_of(&recs, "P-THREE"),
        );
        let s1 = id_of(&recs, "S-ONE");
        let perform = |from: &str, to: &str| serde_json::json!({"type":"calls","from":from,"to":to,"properties":{"kind":"perform"}});
        let mut want = vec![
            perform(&main, &p1),
            perform(&main, &p1),
            perform(&main, &p2),
            perform(&main, &p3),
            serde_json::json!({"type":"calls","from":main,"to":p2,"properties":{"kind":"goto"}}),
            serde_json::json!({"type":"uses","from":p3,"to":s1}),
        ];
        want.sort_by_key(|v| v.to_string());
        assert_eq!(edges(&recs, &["calls", "uses"]), want);
    }

    #[test]
    fn unresolved_perform_is_an_unknown_target_with_a_diagnostic() {
        let text = control().replace("PERFORM S-ONE.", "PERFORM NOPE.");
        let (recs, diags) = scan_text("/root/prog.cob", &text, &[]);
        let p3 = id_of(&recs, "P-THREE");
        assert!(recs
            .contains(&serde_json::json!({"type":"unresolved","fqn":"NOPE","category":"unknown"})));
        assert!(recs.contains(&serde_json::json!({"type":"unresolved_call","from":p3,"to":"NOPE"})));
        assert!(!recs.iter().any(|r| r["type"] == "uses"));
        assert!(diags
            .iter()
            .any(|d| format!("{d:?}").contains("perform NOPE")));
    }

    /// A one-paragraph-performing program `name`, its MAIN-PARA calling P-A.
    fn perform_prog(name: &str) -> String {
        [
            at(8, "IDENTIFICATION DIVISION."),
            at(8, &format!("PROGRAM-ID. {name}.")),
            at(8, "PROCEDURE DIVISION."),
            at(8, "MAIN-PARA."),
            at(12, "PERFORM P-A."),
            at(8, "P-A."),
            at(12, "DISPLAY X."),
        ]
        .join("\n")
    }

    #[test]
    fn targets_drop_control_transfers_whose_source_is_outside_them() {
        let (a, b) = (perform_prog("A"), perform_prog("B"));
        let tree = [("/root/a.cob", a.as_str()), ("/root/b.cob", b.as_str())];
        let mut emitter = scan_tree(&tree);
        assert_eq!(edges(&jsonl(&emitter), &["calls"]).len(), 2);
        let target: std::collections::HashSet<String> =
            ["/root/a.cob".to_string()].into_iter().collect();
        emitter.retain_targets(&target);
        let recs = jsonl(&emitter);
        let main_a = id_of(&recs, "MAIN-PARA");
        let calls = edges(&recs, &["calls"]);
        assert_eq!(calls.len(), 1, "{calls:?}");
        assert_eq!(calls[0]["from"], main_a.as_str());
    }
}
