//! End-to-end tests for the COBOL reader over real files: COPY search, copybook
//! extensions, nesting limits, circular and missing copybooks, source-map positions in
//! copybooks, `>>SOURCE` format detection, the free-format warning, the diagnostic line
//! format and recovery. Every case writes its own scratch directory under the platform temp
//! dir and carries `#[ignore]`; run via `cargo test e2e:: -- --ignored --test-threads=4`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use cobolfrontend::ast::CompilationUnit;
use cobolfrontend::diag::{write_diagnostic, Diagnostic};
use cobolfrontend::dialect::Dialect;
use cobolfrontend::parser::parse_source;
use cobolfrontend::preprocess::{read_file, Search};

static NEXT: AtomicU64 = AtomicU64::new(0);

/// A scratch tree under the platform temp dir, removed on drop.
struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "cobollib-e2e-{}-{tag}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create scratch root");
        Scratch { root }
    }

    /// Writes `text` at `rel` under the root, creating parent directories.
    fn write(&self, rel: &str, text: &str) -> PathBuf {
        let path = self.root.join(rel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("create scratch dirs");
        std::fs::write(&path, text).expect("write scratch file");
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// A free-format program whose WORKING-STORAGE holds `body` on its own line (line 6).
fn main_with(body: &str) -> String {
    format!(
        ">>SOURCE FREE\nIDENTIFICATION DIVISION.\nPROGRAM-ID. P.\nDATA DIVISION.\nWORKING-STORAGE SECTION.\n{body}\nPROCEDURE DIVISION.\nEND PROGRAM P.\n"
    )
}

/// Writes `main_rel` under the scratch root, reads it back from disk and parses it through
/// the real reader (`read_file` as the copybook lookup). Returns the unit, the diagnostics
/// and the main file's path.
fn parse(
    scratch: &Scratch,
    main_rel: &str,
    text: &str,
    copy_paths: Vec<PathBuf>,
) -> (CompilationUnit, Vec<Diagnostic>, PathBuf) {
    let main = scratch.write(main_rel, text);
    let text = std::fs::read_to_string(&main).expect("read main");
    let search = Search {
        scan_root: scratch.root.clone(),
        copy_paths,
    };
    let (unit, diags) = parse_source(
        &main.display().to_string(),
        &text,
        Dialect::Cobol85,
        &search,
        &read_file,
    );
    (unit, diags, main)
}

/// The data-item names of the first program's WORKING-STORAGE, joined by commas.
fn data_names(unit: &CompilationUnit) -> String {
    let data = unit.programs[0].data.as_ref().expect("data division");
    data.sections[0]
        .entries
        .iter()
        .filter_map(|e| e.name.as_ref().map(|n| n.text.clone()))
        .collect::<Vec<_>>()
        .join(",")
}

/// Writes one copybook `CB.cpy` per `(dir, marker)` pair (`dir` ends in `/` or is empty),
/// COPYs it from `src/main.cob`, and returns the data name it resolved to.
fn resolved(files: &[(&str, &str)], copy_paths: &[&str]) -> String {
    let scratch = Scratch::new("order");
    for (dir, marker) in files {
        scratch.write(
            &format!("{dir}CB.cpy"),
            &format!("       01 {marker} PIC X.\n"),
        );
    }
    let copy_paths = copy_paths.iter().map(|p| scratch.root.join(p)).collect();
    let (unit, diags, _) = parse(&scratch, "src/main.cob", &main_with("COPY CB."), copy_paths);
    assert!(diags.is_empty(), "{diags:?}");
    data_names(&unit)
}

mod e2e {
    use super::*;

    #[test]
    #[ignore = "e2e tier: real filesystem; run via cargo test e2e:: -- --ignored"]
    fn copy_path_is_searched_after_the_standard_directories() {
        assert_eq!(resolved(&[("extra/", "EXTRA")], &["extra/"]), "EXTRA");
    }

    #[test]
    #[ignore = "e2e tier: real filesystem; run via cargo test e2e:: -- --ignored"]
    fn copy_directory_beats_copy_paths() {
        let found = resolved(&[("extra/", "EXTRA"), ("copy/", "FROM-COPY")], &["extra/"]);
        assert_eq!(found, "FROM-COPY");
    }

    #[test]
    #[ignore = "e2e tier: real filesystem; run via cargo test e2e:: -- --ignored"]
    fn copybooks_directory_beats_copy_directory() {
        let found = resolved(&[("copy/", "FROM-COPY"), ("copybooks/", "BOOKS")], &[]);
        assert_eq!(found, "BOOKS");
    }

    #[test]
    #[ignore = "e2e tier: real filesystem; run via cargo test e2e:: -- --ignored"]
    fn scan_root_beats_copybooks_directory() {
        let found = resolved(&[("copybooks/", "BOOKS"), ("", "ROOT")], &[]);
        assert_eq!(found, "ROOT");
    }

    #[test]
    #[ignore = "e2e tier: real filesystem; run via cargo test e2e:: -- --ignored"]
    fn including_directory_is_searched_first() {
        let found = resolved(&[("", "ROOT"), ("src/", "INCLUDING")], &[]);
        assert_eq!(found, "INCLUDING");
    }

    #[test]
    #[ignore = "e2e tier: real filesystem; run via cargo test e2e:: -- --ignored"]
    fn copy_paths_are_searched_in_the_order_given() {
        let first = resolved(
            &[("extra/", "FIRST"), ("extra2/", "SECOND")],
            &["extra/", "extra2/"],
        );
        let second = resolved(
            &[("extra/", "FIRST"), ("extra2/", "SECOND")],
            &["extra2/", "extra/"],
        );
        assert_eq!((first.as_str(), second.as_str()), ("FIRST", "SECOND"));
    }

    #[test]
    #[ignore = "e2e tier: real filesystem; run via cargo test e2e:: -- --ignored"]
    fn copybook_extension_variants_are_found() {
        for ext in [
            "", ".cpy", ".CPY", ".cbl", ".CBL", ".cob", ".COB", ".cobol", ".COBOL",
        ] {
            let scratch = Scratch::new("ext");
            scratch.write(&format!("src/CB{ext}"), "       01 FOUND PIC X.\n");
            let (unit, diags, _) = parse(&scratch, "src/main.cob", &main_with("COPY CB."), vec![]);
            assert!(diags.is_empty(), "ext `{ext}`: {diags:?}");
            assert_eq!(data_names(&unit), "FOUND", "ext `{ext}`");
        }
    }

    /// A chain of `n` copybooks `C1.cpy` … `Cn.cpy`, each COPYing the next; `Cn` holds `LEAF`.
    fn chain(scratch: &Scratch, n: usize) {
        for k in 1..=n {
            let body = if k < n {
                format!("           COPY C{}.\n", k + 1)
            } else {
                "       01 LEAF PIC X.\n".to_string()
            };
            scratch.write(&format!("C{k}.cpy"), &body);
        }
    }

    #[test]
    #[ignore = "e2e tier: real filesystem; run via cargo test e2e:: -- --ignored"]
    fn copy_nesting_at_the_depth_limit_is_accepted() {
        let scratch = Scratch::new("depth-ok");
        chain(&scratch, 10);
        let (unit, diags, _) = parse(&scratch, "src/main.cob", &main_with("COPY C1."), vec![]);
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(data_names(&unit), "LEAF");
    }

    #[test]
    #[ignore = "e2e tier: real filesystem; run via cargo test e2e:: -- --ignored"]
    fn copy_nesting_past_the_depth_limit_is_refused_at_the_copy() {
        let scratch = Scratch::new("depth-over");
        chain(&scratch, 11);
        let (unit, diags, _) = parse(&scratch, "src/main.cob", &main_with("COPY C1."), vec![]);
        let refused: Vec<_> = diags
            .iter()
            .filter(|d| d.message == "COPY nests deeper than 10 levels")
            .collect();
        assert_eq!(refused.len(), 1, "{diags:?}");
        assert!(refused[0].file.ends_with("C10.cpy"), "{refused:?}");
        assert_eq!((refused[0].line, refused[0].col), (1, 12));
        assert_eq!(data_names(&unit), "");
    }

    #[test]
    #[ignore = "e2e tier: real filesystem; run via cargo test e2e:: -- --ignored"]
    fn circular_copy_is_refused_at_the_closing_copy() {
        let scratch = Scratch::new("circular");
        scratch.write("A.cpy", "           COPY B.\n");
        scratch.write("B.cpy", "           COPY A.\n");
        let (_, diags, _) = parse(&scratch, "src/main.cob", &main_with("COPY A."), vec![]);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].message, "circular COPY of `A`");
        assert!(diags[0].file.ends_with("B.cpy"), "{diags:?}");
        assert_eq!((diags[0].line, diags[0].col), (1, 12));
    }

    #[test]
    #[ignore = "e2e tier: real filesystem; run via cargo test e2e:: -- --ignored"]
    fn missing_copybook_is_diagnosed_at_the_copy_and_the_scan_continues() {
        let scratch = Scratch::new("missing");
        let text = format!(
            "{}IDENTIFICATION DIVISION.\nPROGRAM-ID. Q.\nEND PROGRAM Q.\n",
            main_with("COPY MISSING.")
        );
        let (unit, diags, main) = parse(&scratch, "src/main.cob", &text, vec![]);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].file, main.display().to_string());
        assert_eq!((diags[0].line, diags[0].col), (6, 1));
        assert_eq!(diags[0].message, "copybook `MISSING` not found");
        let names: Vec<&str> = unit.programs.iter().map(|p| p.name.text.as_str()).collect();
        assert_eq!(names, ["P", "Q"]);
    }

    #[test]
    #[ignore = "e2e tier: real filesystem; run via cargo test e2e:: -- --ignored"]
    fn copybook_diagnostics_point_into_the_copybook_file() {
        let scratch = Scratch::new("map");
        let copybook = scratch.write(
            "CB.cpy",
            ">>SOURCE FORMAT COBOL85\n       01 FIRST PIC X.\n           01 BAD PIC X.\n",
        );
        let (_, diags, _) = parse(&scratch, "src/main.cob", &main_with("COPY CB."), vec![]);
        let at = diags
            .iter()
            .find(|d| d.message.contains("must start in area A"))
            .unwrap_or_else(|| panic!("no area A diagnostic: {diags:?}"));
        assert_eq!(at.file, copybook.display().to_string());
        assert_eq!((at.line, at.col), (3, 12));
    }

    #[test]
    #[ignore = "e2e tier: real filesystem; run via cargo test e2e:: -- --ignored"]
    fn source_free_directive_reads_text_past_column_80_without_warning() {
        let scratch = Scratch::new("free");
        let long = format!("{}01 W PIC X.", " ".repeat(80));
        let text = format!(
            ">>SOURCE FREE\nIDENTIFICATION DIVISION.\nPROGRAM-ID. P.\nDATA DIVISION.\nWORKING-STORAGE SECTION.\n{long}\nEND PROGRAM P.\n"
        );
        let (unit, diags, _) = parse(&scratch, "src/main.cob", &text, vec![]);
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(data_names(&unit), "W");
    }

    #[test]
    #[ignore = "e2e tier: real filesystem; run via cargo test e2e:: -- --ignored"]
    fn unmarked_free_looking_file_warns_naming_the_file() {
        let scratch = Scratch::new("warn");
        let long = format!("{}Y", " ".repeat(80));
        let text = format!("       IDENTIFICATION DIVISION.\n       PROGRAM-ID. P.\n{long}\n       END PROGRAM P.\n");
        let (_, diags, main) = parse(&scratch, "src/main.cob", &text, vec![]);
        let warn = diags
            .iter()
            .find(|d| d.message.contains("free-format"))
            .unwrap_or_else(|| panic!("no free-format warning: {diags:?}"));
        assert_eq!(warn.file, main.display().to_string());
        assert_eq!((warn.line, warn.col), (3, 1));
    }

    #[test]
    #[ignore = "e2e tier: real filesystem; run via cargo test e2e:: -- --ignored"]
    fn diagnostic_line_is_cobol_file_line_col_message() {
        let scratch = Scratch::new("line");
        let (_, diags, main) = parse(
            &scratch,
            "src/main.cob",
            &main_with("COPY MISSING."),
            vec![],
        );
        let mut out = Vec::new();
        for d in &diags {
            write_diagnostic(&mut out, d).expect("write to vec");
        }
        let expected = format!(
            "cobol: {}:6:1: copybook `MISSING` not found\n",
            main.display()
        );
        assert_eq!(String::from_utf8(out).expect("utf8"), expected);
    }

    #[test]
    #[ignore = "e2e tier: real filesystem; run via cargo test e2e:: -- --ignored"]
    fn recovery_keeps_the_programs_after_a_broken_one() {
        let scratch = Scratch::new("recover");
        let text = ">>SOURCE FREE\nIDENTIFICATION DIVISION.\nAUTHOR. X.\nPROCEDURE DIVISION.\n    STOP RUN.\nIDENTIFICATION DIVISION.\nPROGRAM-ID. GOOD.\nEND PROGRAM GOOD.\n";
        let (unit, diags, _) = parse(&scratch, "src/main.cob", text, vec![]);
        assert_eq!(diags.len(), 1, "{diags:?}");
        let names: Vec<&str> = unit.programs.iter().map(|p| p.name.text.as_str()).collect();
        assert_eq!(names, ["GOOD"]);
    }
}
