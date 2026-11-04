//! End-to-end tests for the IBM and EXEC fixtures under `tests/exec/`: the built `cobolfrontend`
//! scans a scratch copy of the whole directory under `ibm` and under the non-IBM dialects, and the
//! emitted facts and stderr are checked. Each scan writes its own tree under the platform temp dir
//! and removes it on drop; run via `cargo test-e2e`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;

static NEXT: AtomicU64 = AtomicU64::new(0);

/// The diagnostic every EXEC block gives outside `ibm`.
const NOT_IBM: &str = "an EXEC block is recognised only under ibm";

/// Each fixture's EXEC blocks, each one diagnostic outside `ibm` (the DATA DIVISION fixture's
/// INCLUDE and both DECLARE SECTION markers are three).
const EXEC_BLOCKS: [(&str, usize); 13] = [
    ("exec_callee.cob", 0),
    ("exec_link_literal.cob", 2),
    ("exec_xctl_variable.cob", 2),
    ("exec_service.cob", 2),
    ("exec_transid.cob", 2),
    ("exec_sql_hostvars.cob", 1),
    ("exec_sql_call.cob", 2),
    ("exec_sql_function.cob", 1),
    ("exec_nested_if.cob", 2),
    ("exec_after_move.cob", 1),
    ("exec_data_division.cob", 3),
    ("exec_non_ibm_error.cob", 1),
    ("ibm_oo_data_names.cob", 0),
];

/// The unresolved names the EXEC fixtures carry, which a non-IBM scan must not emit.
const EXEC_TARGETS: [&str; 8] = [
    "OUTSIDEPGM",
    "SVCNAME",
    "WSNAME",
    "SQLOUTSIDE",
    "TRN1",
    "TRN2",
    "TRN3",
    "EMPLOYEE",
];

/// A scratch tree under the platform temp dir (canonical, so emitted paths match), removed on drop.
struct Scratch {
    root: PathBuf,
}

impl Scratch {
    /// A fresh tree holding a copy of every `.cob` fixture in `tests/exec/`, at the same name.
    fn exec_samples() -> Self {
        let root = std::env::temp_dir().join(format!(
            "cobollib-exec-e2e-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create scratch root");
        let root = std::fs::canonicalize(&root).expect("canonicalize scratch root");
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/exec");
        for entry in std::fs::read_dir(&fixtures).expect("read fixtures") {
            let path = entry.expect("fixture entry").path();
            if path.extension().is_some_and(|e| e == "cob") {
                let name = path.file_name().expect("fixture name");
                std::fs::copy(&path, root.join(name)).expect("copy fixture");
            }
        }
        Scratch { root }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
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

/// Scans the whole fixture tree under `std`. The run must exit 0: a file with errors is not a
/// crash. Returns its facts and its stderr lines.
fn scan(std: &str) -> (Vec<Value>, Vec<String>) {
    let scratch = Scratch::exec_samples();
    let out = Command::new(env!("CARGO_BIN_EXE_cobolfrontend"))
        .arg(&scratch.root)
        .arg(std)
        .output()
        .expect("spawn cobolfrontend");
    assert!(out.status.success(), "{std}: {:?}", stderr_lines(&out));
    (records(&out), stderr_lines(&out))
}

/// Whether the record's `path` ends with `suffix`.
fn path_ends(r: &Value, suffix: &str) -> bool {
    r["path"].as_str().is_some_and(|p| p.ends_with(suffix))
}

/// The node of `kind` (`struct`/`function`) named `name` under `parent` (`""` at top level).
fn node<'a>(recs: &'a [Value], kind: &str, name: &str, parent: &str) -> &'a Value {
    recs.iter()
        .find(|r| r["type"] == kind && r["name"] == name && r["parent"] == parent)
        .unwrap_or_else(|| panic!("no {kind} {name} under `{parent}`: {recs:?}"))
}

/// The id of the node `node` returns.
fn id(recs: &[Value], kind: &str, name: &str, parent: &str) -> String {
    node(recs, kind, name, parent)["id"]
        .as_str()
        .expect("id")
        .to_string()
}

/// Whether a `kind` edge runs from `from` to `to`.
fn has(recs: &[Value], kind: &str, from: &str, to: &str) -> bool {
    recs.iter()
        .any(|r| r["type"] == kind && r["from"] == from && r["to"] == to)
}

/// The category of the unresolved target `fqn`.
fn category(recs: &[Value], fqn: &str) -> Option<String> {
    recs.iter()
        .find(|r| r["type"] == "unresolved" && r["fqn"] == fqn)
        .and_then(|r| r["category"].as_str().map(str::to_string))
}

/// The `(kind, to)` pairs of the facts `from` emits: its calls, uses and unresolved references.
fn edges_from(recs: &[Value], from: &str) -> Vec<(String, String)> {
    recs.iter()
        .filter(|r| {
            r["from"] == from
                && matches!(
                    r["type"].as_str(),
                    Some("calls" | "uses" | "unresolved_call" | "unresolved_use")
                )
        })
        .map(|r| {
            let kind = r["type"].as_str().unwrap_or_default();
            let to = r["to"].as_str().unwrap_or_default();
            (kind.to_string(), to.to_string())
        })
        .collect()
}

mod e2e {
    use super::*;

    #[test]
    #[ignore = "e2e tier: spawns cobolfrontend over a scratch tree; run via cargo test-e2e"]
    fn ibm_oo_data_names_have_no_error_and_their_statements_give_uses() {
        let (recs, stderr) = scan("-std=ibm");
        assert!(
            !stderr.iter().any(|l| l.contains("ibm_oo_data_names.cob")),
            "{stderr:?}"
        );
        let main = id(&recs, "function", "MAIN-PARA", "IBMOODATA");
        for name in ["OBJECT", "METHOD", "INVOKE"] {
            let data = node(&recs, "struct", name, "IBMOODATA");
            assert!(path_ends(data, "ibm_oo_data_names.cob"), "{name}");
            let data_id = data["id"].as_str().expect("id");
            assert!(has(&recs, "uses", &main, data_id), "{name}");
        }
    }

    #[test]
    #[ignore = "e2e tier: spawns cobolfrontend over a scratch tree; run via cargo test-e2e"]
    fn link_literal_in_scan_is_a_uses_and_outside_is_an_external_call() {
        let (recs, _) = scan("-std=ibm");
        let main = id(&recs, "function", "MAIN-PARA", "EXECLINK");
        let callee = id(&recs, "struct", "EXECCALLEE", "");
        assert!(has(&recs, "uses", &main, &callee));
        assert!(has(&recs, "unresolved_call", &main, "OUTSIDEPGM"));
        assert_eq!(category(&recs, "OUTSIDEPGM").as_deref(), Some("external"));
    }

    #[test]
    #[ignore = "e2e tier: spawns cobolfrontend over a scratch tree; run via cargo test-e2e"]
    fn xctl_literal_is_a_uses_and_variable_program_is_a_func_value_call() {
        let (recs, _) = scan("-std=ibm");
        let main = id(&recs, "function", "MAIN-PARA", "EXECXCTL");
        let callee = id(&recs, "struct", "EXECCALLEE", "");
        assert!(has(&recs, "uses", &main, &callee));
        assert!(has(&recs, "unresolved_call", &main, "WS-PROG"));
        assert_eq!(category(&recs, "WS-PROG").as_deref(), Some("func-value"));
    }

    #[test]
    #[ignore = "e2e tier: spawns cobolfrontend over a scratch tree; run via cargo test-e2e"]
    fn nested_if_link_and_else_xctl_keep_the_if_and_else_statements() {
        let (recs, _) = scan("-std=ibm");
        let main = id(&recs, "function", "MAIN-PARA", "EXECNEST");
        let callee = id(&recs, "struct", "EXECCALLEE", "");
        let flag = id(&recs, "struct", "WS-FLAG", "EXECNEST");
        assert!(has(&recs, "uses", &main, &callee));
        assert!(has(&recs, "unresolved_call", &main, "OUTSIDEPGM"));
        assert_eq!(category(&recs, "OUTSIDEPGM").as_deref(), Some("external"));
        // The `IF WS-FLAG = 1` condition is a reference of the statement it opens.
        assert!(has(&recs, "uses", &main, &flag));
    }

    #[test]
    #[ignore = "e2e tier: spawns cobolfrontend over a scratch tree; run via cargo test-e2e"]
    fn ibm_services_are_external_unresolved_calls_and_no_uses() {
        let (recs, _) = scan("-std=ibm");
        let main = id(&recs, "function", "MAIN-PARA", "EXECSVC");
        assert!(has(&recs, "unresolved_call", &main, "SVCNAME"));
        assert!(has(&recs, "unresolved_call", &main, "WSNAME"));
        assert_eq!(category(&recs, "SVCNAME").as_deref(), Some("external"));
        assert_eq!(category(&recs, "WSNAME").as_deref(), Some("external"));
        assert!(edges_from(&recs, &main).iter().all(|(k, _)| k != "uses"));
    }

    #[test]
    #[ignore = "e2e tier: spawns cobolfrontend over a scratch tree; run via cargo test-e2e"]
    fn transid_gives_no_edge_and_no_target() {
        let (recs, _) = scan("-std=ibm");
        let main = id(&recs, "function", "MAIN-PARA", "EXECTRN");
        assert!(edges_from(&recs, &main).is_empty());
        for trn in ["TRN1", "TRN2"] {
            assert!(!recs
                .iter()
                .any(|r| r["type"] == "unresolved" && r["fqn"] == trn));
        }
    }

    #[test]
    #[ignore = "e2e tier: spawns cobolfrontend over a scratch tree; run via cargo test-e2e"]
    fn sql_host_variables_are_uses_and_the_indicator_and_table_are_not() {
        let (recs, _) = scan("-std=ibm");
        let main = id(&recs, "function", "MAIN-PARA", "EXECHV");
        let salary = id(&recs, "struct", "WS-SALARY", "EXECHV");
        let empno = id(&recs, "struct", "WS-EMPNO", "EXECHV");
        let indicator = id(&recs, "struct", "WS-SAL-IND", "EXECHV");
        assert!(has(&recs, "uses", &main, &salary));
        assert!(has(&recs, "uses", &main, &empno));
        assert!(!has(&recs, "uses", &main, &indicator));
        assert!(!recs
            .iter()
            .any(|r| r["type"] == "struct" && r["name"] == "EMPLOYEE"));
        assert!(edges_from(&recs, &main)
            .iter()
            .all(|(_, to)| to != "EMPLOYEE"));
    }

    #[test]
    #[ignore = "e2e tier: spawns cobolfrontend over a scratch tree; run via cargo test-e2e"]
    fn sql_functions_and_table_names_leave_no_record_or_target() {
        let (recs, _) = scan("-std=ibm");
        let main = id(&recs, "function", "MAIN-PARA", "EXECSQLF");
        let cnt = id(&recs, "struct", "WS-CNT", "EXECSQLF");
        let up = id(&recs, "struct", "WS-UP", "EXECSQLF");
        assert!(has(&recs, "uses", &main, &cnt));
        assert!(has(&recs, "uses", &main, &up));
        for name in ["COUNT", "UPPER", "LASTNAME", "EMPLOYEE"] {
            assert!(
                edges_from(&recs, &main).iter().all(|(_, to)| to != name),
                "{name}"
            );
        }
    }

    #[test]
    #[ignore = "e2e tier: spawns cobolfrontend over a scratch tree; run via cargo test-e2e"]
    fn sql_call_in_scan_is_a_uses_and_outside_is_an_external_call() {
        let (recs, _) = scan("-std=ibm");
        let main = id(&recs, "function", "MAIN-PARA", "EXECSQLC");
        let callee = id(&recs, "struct", "EXECCALLEE", "");
        assert!(has(&recs, "uses", &main, &callee));
        assert!(has(&recs, "unresolved_call", &main, "SQLOUTSIDE"));
        assert_eq!(category(&recs, "SQLOUTSIDE").as_deref(), Some("external"));
    }

    #[test]
    #[ignore = "e2e tier: spawns cobolfrontend over a scratch tree; run via cargo test-e2e"]
    fn statement_after_a_move_in_one_sentence_keeps_its_uses_and_no_transid_fact() {
        let (recs, _) = scan("-std=ibm");
        let main = id(&recs, "function", "MAIN-PARA", "EXECMOVE");
        let code = id(&recs, "struct", "WS-CODE", "EXECMOVE");
        assert!(has(&recs, "uses", &main, &code));
        assert!(edges_from(&recs, &main).iter().all(|(_, to)| to != "TRN3"));
    }

    #[test]
    #[ignore = "e2e tier: spawns cobolfrontend over a scratch tree; run via cargo test-e2e"]
    fn data_division_exec_has_no_error_and_its_declare_section_records_stay() {
        let (recs, stderr) = scan("-std=ibm");
        assert!(
            !stderr.iter().any(|l| l.contains("exec_data_division.cob")),
            "{stderr:?}"
        );
        for name in ["HV-NAME", "HV-ID"] {
            assert!(path_ends(
                node(&recs, "struct", name, "EXECDATA"),
                "exec_data_division.cob"
            ));
        }
    }

    #[test]
    #[ignore = "e2e tier: spawns cobolfrontend over a scratch tree; run via cargo test-e2e"]
    fn ibm_fixture_tree_has_zero_files_with_errors() {
        let (_, stderr) = scan("-std=ibm");
        assert_eq!(
            stderr.last().map(String::as_str),
            Some("cobol: 0 files had errors"),
            "{stderr:?}"
        );
    }

    #[test]
    #[ignore = "e2e tier: spawns cobolfrontend over a scratch tree; run via cargo test-e2e"]
    fn cobol85_gives_one_diagnostic_per_exec_block_and_no_exec_facts() {
        let (recs, stderr) = scan("-std=cobol85");
        for (file, blocks) in EXEC_BLOCKS {
            let n = stderr
                .iter()
                .filter(|l| l.contains(file) && l.contains(NOT_IBM))
                .count();
            assert_eq!(n, blocks, "{file}: {stderr:?}");
        }
        // Eleven fixtures carry an EXEC block; the run exits 0 and the count is the last line.
        assert_eq!(
            stderr.last().map(String::as_str),
            Some("cobol: 11 files had errors"),
            "{stderr:?}"
        );
        let callee = id(&recs, "struct", "EXECCALLEE", "");
        assert!(!recs
            .iter()
            .any(|r| r["type"] == "uses" && r["to"] == callee.as_str()));
        assert!(!recs.iter().any(|r| r["type"] == "unresolved"
            && EXEC_TARGETS.contains(&r["fqn"].as_str().unwrap_or_default())));
        // The DATA DIVISION entries survive the skipped blocks.
        node(&recs, "struct", "HV-NAME", "EXECDATA");
    }

    #[test]
    #[ignore = "e2e tier: spawns cobolfrontend over a scratch tree; run via cargo test-e2e"]
    fn non_ibm_error_block_is_skipped_and_its_statements_stay_intact() {
        for std in ["-std=cobol85", "-std=cobol2002", "-std=cobol2014"] {
            let (recs, stderr) = scan(std);
            let errors = stderr
                .iter()
                .filter(|l| l.contains("exec_non_ibm_error.cob") && l.contains(NOT_IBM))
                .count();
            assert_eq!(errors, 1, "{std}: {stderr:?}");
            let main = id(&recs, "function", "MAIN-PARA", "EXECNON");
            let other = id(&recs, "function", "OTHER-PARA", "EXECNON");
            let ws_x = id(&recs, "struct", "WS-X", "EXECNON");
            let callee = id(&recs, "struct", "EXECCALLEE", "");
            assert!(has(&recs, "uses", &main, &ws_x), "{std}");
            assert!(has(&recs, "calls", &main, &other), "{std}");
            assert!(!has(&recs, "uses", &main, &callee), "{std}");
        }
        let (recs, _) = scan("-std=ibm");
        let main = id(&recs, "function", "MAIN-PARA", "EXECNON");
        let callee = id(&recs, "struct", "EXECCALLEE", "");
        assert!(has(&recs, "uses", &main, &callee));
    }
}
