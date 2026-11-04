//! End-to-end tests for the `cobolfrontend` binary over real scratch trees: the emitted
//! records, the diagnostic summary line, the dialect refusal, source claims and `--targets`.
//! Each test writes its own scratch tree under the platform temp dir and removes it on drop;
//! run via `cargo test e2e:: -- --ignored --test-threads=4`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;

static NEXT: AtomicU64 = AtomicU64::new(0);

/// A scratch tree under the platform temp dir (canonical, so emitted paths match), removed on drop.
struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "cobollib-scan-e2e-{}-{tag}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create scratch root");
        let root = std::fs::canonicalize(&root).expect("canonicalize scratch root");
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

/// Joins `lines` into a source text with a trailing newline.
fn source(lines: &[&str]) -> String {
    let mut text = lines.join("\n");
    text.push('\n');
    text
}

/// The main program: a copybook record, a section, paragraphs and PERFORM, GO TO and CALL.
fn program() -> String {
    source(&[
        ">>SOURCE FREE",
        "IDENTIFICATION DIVISION.",
        "PROGRAM-ID. PROG.",
        "DATA DIVISION.",
        "WORKING-STORAGE SECTION.",
        "COPY REC.",
        "PROCEDURE DIVISION.",
        "MAIN-SECTION SECTION.",
        "START-PARA.",
        "    PERFORM WORK-PARA.",
        "    GO TO END-PARA.",
        "WORK-PARA.",
        "    CALL \"SUB\".",
        "END-PARA.",
        "    STOP RUN.",
        "END PROGRAM PROG.",
    ])
}

/// A standalone called program.
fn sub() -> String {
    source(&[
        ">>SOURCE FREE",
        "IDENTIFICATION DIVISION.",
        "PROGRAM-ID. SUB.",
        "PROCEDURE DIVISION.",
        "SUB-PARA.",
        "    STOP RUN.",
        "END PROGRAM SUB.",
    ])
}

/// The shared layout: `prog.CBL` (COPYs `REC.cpy`, CALLs `SUB`) and `sub.cbl`.
fn layout(scratch: &Scratch) {
    scratch.write("prog.CBL", &program());
    scratch.write("REC.cpy", "       01 REC-CB PIC X.\n");
    scratch.write("sub.cbl", &sub());
}

/// Runs the built `cobolfrontend` over `root` with `args`.
fn run(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_cobolfrontend"))
        .arg(root)
        .args(args)
        .output()
        .expect("spawn cobolfrontend")
}

/// stdout as one JSON record per line.
fn records(out: &Output) -> Vec<Value> {
    String::from_utf8(out.stdout.clone())
        .expect("utf8 stdout")
        .lines()
        .map(|l| serde_json::from_str(l).expect("json record"))
        .collect()
}

/// The stderr lines.
fn stderr_lines(out: &Output) -> Vec<String> {
    String::from_utf8(out.stderr.clone())
        .expect("utf8 stderr")
        .lines()
        .map(str::to_string)
        .collect()
}

/// The record of `kind` (`struct`/`function`) named `name`.
fn named<'a>(recs: &'a [Value], kind: &str, name: &str) -> &'a Value {
    recs.iter()
        .find(|r| r["type"] == kind && r["name"] == name)
        .unwrap_or_else(|| panic!("no {kind} {name}: {recs:?}"))
}

/// The id of the record of `kind` named `name`.
fn id_of(recs: &[Value], kind: &str, name: &str) -> String {
    named(recs, kind, name)["id"]
        .as_str()
        .expect("id")
        .to_string()
}

/// Whether a `calls` edge of `kind` runs from `from` to `to`.
fn calls(recs: &[Value], from: &str, to: &str, kind: &str) -> bool {
    recs.iter().any(|r| {
        r["type"] == "calls"
            && r["from"] == from
            && r["to"] == to
            && r["properties"]["kind"] == kind
    })
}

/// Whether a `uses` edge runs from `from` to `to` (an endpoint string as emitted).
fn uses(recs: &[Value], from: &str, to: &str) -> bool {
    recs.iter()
        .any(|r| r["type"] == "uses" && r["from"] == from && r["to"] == to)
}

/// Whether the record's `path` ends with `suffix`.
fn path_ends(r: &Value, suffix: &str) -> bool {
    r["path"].as_str().is_some_and(|p| p.ends_with(suffix))
}

mod e2e {
    use super::*;

    #[test]
    #[ignore = "e2e tier: spawns cobolfrontend over a scratch tree; run via cargo test e2e:: -- --ignored"]
    fn copybook_is_a_file_with_its_located_declarations() {
        let scratch = Scratch::new("copybook");
        layout(&scratch);
        let out = run(&scratch.root, &[]);
        assert!(out.status.success(), "{:?}", stderr_lines(&out));
        let recs = records(&out);
        let copybook = recs
            .iter()
            .find(|r| r["type"] == "file" && path_ends(r, "REC.cpy"))
            .unwrap_or_else(|| panic!("no copybook File: {recs:?}"));
        assert_eq!(copybook["end_line"], 1);
        let rec = named(&recs, "struct", "REC-CB");
        assert!(path_ends(rec, "REC.cpy"), "{rec:?}");
        assert_eq!(
            (rec["start_line"].as_u64(), rec["end_line"].as_u64()),
            (Some(1), Some(1))
        );
        assert_eq!(rec["parent"], "PROG");
    }

    #[test]
    #[ignore = "e2e tier: spawns cobolfrontend over a scratch tree; run via cargo test e2e:: -- --ignored"]
    fn program_section_paragraph_and_record_structs_are_emitted() {
        let scratch = Scratch::new("structs");
        layout(&scratch);
        let out = run(&scratch.root, &[]);
        assert!(out.status.success(), "{:?}", stderr_lines(&out));
        let recs = records(&out);
        let prog = named(&recs, "struct", "PROG");
        assert_eq!(prog["parent"], "");
        assert!(path_ends(prog, "prog.CBL"), "{prog:?}");
        assert_eq!(named(&recs, "struct", "SUB")["parent"], "");
        let section = named(&recs, "struct", "MAIN-SECTION");
        assert_eq!(section["parent"], "PROG");
        for (para, line) in [("START-PARA", 9), ("WORK-PARA", 12), ("END-PARA", 14)] {
            let f = named(&recs, "function", para);
            assert_eq!(f["parent"], "PROG.MAIN-SECTION", "{para}");
            assert_eq!(f["start_line"], line, "{para}");
            assert!(path_ends(f, "prog.CBL"), "{para}");
        }
    }

    #[test]
    #[ignore = "e2e tier: spawns cobolfrontend over a scratch tree; run via cargo test e2e:: -- --ignored"]
    fn perform_goto_and_call_edges_are_emitted() {
        let scratch = Scratch::new("edges");
        layout(&scratch);
        let out = run(&scratch.root, &[]);
        assert!(out.status.success(), "{:?}", stderr_lines(&out));
        let recs = records(&out);
        let start = id_of(&recs, "function", "START-PARA");
        let work = id_of(&recs, "function", "WORK-PARA");
        let end = id_of(&recs, "function", "END-PARA");
        let sub = id_of(&recs, "struct", "SUB");
        assert!(
            calls(&recs, &start, &work, "perform"),
            "PERFORM edge missing"
        );
        assert!(calls(&recs, &start, &end, "goto"), "GO TO edge missing");
        assert!(uses(&recs, &work, &sub), "CALL edge missing");
    }

    #[test]
    #[ignore = "e2e tier: spawns cobolfrontend over a scratch tree; run via cargo test e2e:: -- --ignored"]
    fn error_count_is_the_last_stderr_line_on_success_with_errors() {
        let scratch = Scratch::new("errors");
        layout(&scratch);
        scratch.write(
            "bad.cbl",
            &source(&[
                ">>SOURCE FREE",
                "IDENTIFICATION DIVISION.",
                "PROGRAM-ID. BAD.",
                "DATA DIVISION.",
                "WORKING-STORAGE SECTION.",
                "COPY MISSING.",
                "PROCEDURE DIVISION.",
                "BAD-PARA.",
                "    STOP RUN.",
                "END PROGRAM BAD.",
            ]),
        );
        let out = run(&scratch.root, &[]);
        assert!(out.status.success(), "{:?}", stderr_lines(&out));
        let stderr = stderr_lines(&out);
        assert!(
            stderr
                .iter()
                .any(|l| l.contains("copybook `MISSING` not found")),
            "{stderr:?}"
        );
        assert_eq!(
            stderr.last().map(String::as_str),
            Some("cobol: 1 file had errors"),
            "{stderr:?}"
        );
        assert!(!records(&out).is_empty());
    }

    #[test]
    #[ignore = "e2e tier: spawns cobolfrontend over a scratch tree; run via cargo test e2e:: -- --ignored"]
    fn cobol2023_is_refused_with_a_non_zero_exit_and_last_stderr_line() {
        let scratch = Scratch::new("cobol2023");
        layout(&scratch);
        let out = run(&scratch.root, &["-std=cobol2023"]);
        assert!(!out.status.success());
        assert!(out.stdout.is_empty(), "{:?}", records(&out));
        assert_eq!(
            stderr_lines(&out).last().map(String::as_str),
            Some("cobolfrontend: dialect cobol2023 is a placeholder with no features yet; use cobol85, cobol2002, cobol2014 or ibm")
        );
    }

    #[test]
    #[ignore = "e2e tier: spawns cobolfrontend over a scratch tree; run via cargo test e2e:: -- --ignored"]
    fn source_and_copybook_extensions_are_claimed_in_mixed_case() {
        let scratch = Scratch::new("claim");
        scratch.write(
            "a.CBL",
            &source(&[
                ">>SOURCE FREE",
                "IDENTIFICATION DIVISION.",
                "PROGRAM-ID. AONE.",
                "END PROGRAM AONE.",
            ]),
        );
        scratch.write(
            "b.Cbl",
            &source(&[
                ">>SOURCE FREE",
                "IDENTIFICATION DIVISION.",
                "PROGRAM-ID. BTWO.",
                "END PROGRAM BTWO.",
            ]),
        );
        scratch.write("c.CPY", "       01 C-REC PIC X.\n");
        scratch.write("d.cpy", "       01 D-REC PIC X.\n");
        scratch.write("e.txt", "not cobol\n");
        let out = run(&scratch.root, &[]);
        assert!(out.status.success(), "{:?}", stderr_lines(&out));
        let recs = records(&out);
        for name in ["a.CBL", "b.Cbl", "c.CPY", "d.cpy"] {
            assert!(
                recs.iter()
                    .any(|r| r["type"] == "file" && path_ends(r, name)),
                "no File for {name}: {recs:?}"
            );
        }
        assert!(!recs
            .iter()
            .any(|r| r["type"] == "file" && path_ends(r, "e.txt")));
        assert_eq!(named(&recs, "struct", "AONE")["parent"], "");
        assert_eq!(named(&recs, "struct", "BTWO")["parent"], "");
    }

    #[test]
    #[ignore = "e2e tier: spawns cobolfrontend over a scratch tree; run via cargo test e2e:: -- --ignored"]
    fn targets_keep_the_target_facts_and_cross_file_edges_unrooted() {
        let scratch = Scratch::new("targets");
        let prog = scratch.write(
            "prog.cbl",
            &source(&[
                ">>SOURCE FREE",
                "IDENTIFICATION DIVISION.",
                "PROGRAM-ID. PROG.",
                "PROCEDURE DIVISION.",
                "START-PARA.",
                "    PERFORM LOCAL-PARA.",
                "    CALL \"OTHER\".",
                "    CALL \"SUB-B\".",
                "LOCAL-PARA.",
                "    STOP RUN.",
                "END PROGRAM PROG.",
            ]),
        );
        scratch.write(
            "other.cbl",
            &source(&[
                ">>SOURCE FREE",
                "IDENTIFICATION DIVISION.",
                "PROGRAM-ID. OTHER.",
                "END PROGRAM OTHER.",
            ]),
        );
        scratch.write(
            "src/cobol/sub-b.cbl",
            &source(&[
                ">>SOURCE FREE",
                "IDENTIFICATION DIVISION.",
                "PROGRAM-ID. SUB-B.",
                "END PROGRAM SUB-B.",
            ]),
        );
        let list = scratch.write("targets.txt", &format!("{}\n", prog.display()));
        let list = list.display().to_string();
        let out = run(&scratch.root, &["--targets", &list]);
        assert!(out.status.success(), "{:?}", stderr_lines(&out));
        let recs = records(&out);

        let files: Vec<&Value> = recs.iter().filter(|r| r["type"] == "file").collect();
        assert_eq!(files.len(), 1, "{files:?}");
        assert!(path_ends(files[0], "prog.cbl"));
        assert!(recs
            .iter()
            .filter(|r| matches!(r["type"].as_str(), Some("struct" | "function")))
            .all(|r| path_ends(r, "prog.cbl")));
        assert!(!recs
            .iter()
            .any(|r| r["name"] == "SUB-B" || r["name"] == "OTHER"));

        let start = id_of(&recs, "function", "START-PARA");
        let local = id_of(&recs, "function", "LOCAL-PARA");
        assert!(calls(&recs, &start, &local, "perform"));
        assert!(uses(&recs, &start, "OTHER"), "{recs:?}");
        assert!(uses(&recs, &start, "src/cobol.SUB-B"), "{recs:?}");
    }
}
