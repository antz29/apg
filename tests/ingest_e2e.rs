mod common;

use apg::graph::NodeKind;
use apg::ingest::*;
use apg::layers::NodeProperties;
use apg::schema::{Record, SCAN_HEAD};
use apg::testutil::read_graph_jsonl;
use std::collections::BTreeSet;
use std::path::Path;

fn srec(id: &str, parent: &str, name: &str, path: &str) -> Record {
    Record::Struct {
        id: id.to_string(),
        parent: parent.to_string(),
        name: name.to_string(),
        path: path.to_string(),
        start: 0,
        end: 1,
        start_line: 1,
        end_line: 1,
    }
}

fn frec(id: &str, parent: &str, name: &str, path: &str) -> Record {
    Record::Function {
        id: id.to_string(),
        parent: parent.to_string(),
        name: name.to_string(),
        params: vec![],
        file: path.to_string(),
        path: path.to_string(),
        start: 0,
        end: 1,
        start_line: 1,
        end_line: 1,
    }
}

fn file_rec(path: &str, parent: &str, end_line: u32) -> Record {
    Record::File {
        path: path.to_string(),
        parent: parent.to_string(),
        start_line: 1,
        end_line,
    }
}

// -----------------------------------------------------------------------
// Real-DB oracle readers (phase-03 task-9). Non-#[test] harness helpers, so
// they live at the `mod tests` root and are shared by the whole module.
// -----------------------------------------------------------------------

/// Runs `query` against a DB file opened read-only and returns every row's
/// cells as strings (empty when the DB cannot be opened/queried).
fn db_rows(path: &Path, query: &str) -> Vec<Vec<String>> {
    let db = lbug::Database::new(path, lbug::SystemConfig::default().read_only(true))
        .unwrap_or_else(|e| panic!("open {}: {e}", path.display()));
    let conn = lbug::Connection::new(&db).unwrap();
    let rows = conn
        .query(query)
        .map(|r| {
            r.map(|row| row.iter().map(|v| v.to_string()).collect::<Vec<String>>())
                .collect::<Vec<Vec<String>>>()
        })
        .unwrap_or_default();
    drop(conn);
    drop(db);
    rows
}

/// Every table's row count in a DB file opened read-only — the per-label
/// NODE counts AND the per-rel-type COUNTS in one map (`show_tables()`
/// enumerates both node and REL tables).
fn db_table_counts(path: &Path) -> std::collections::BTreeMap<String, i64> {
    let mut out = std::collections::BTreeMap::new();
    let tables = db_rows(path, "CALL show_tables() RETURN name, type");
    for row in tables {
        let table = row.first().cloned().unwrap_or_default();
        let kind = row.get(1).cloned().unwrap_or_default();
        let q = if kind == "REL" {
            format!("MATCH ()-[r:{table}]->() RETURN count(*)")
        } else {
            format!("MATCH (n:{table}) RETURN count(*)")
        };
        let n = db_rows(path, &q)
            .first()
            .and_then(|r| r.first())
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(-1);
        out.insert(table, n);
    }
    out
}

/// The `UnresolvedTarget` rows of a DB file as `(fqn, category)`.
fn db_unresolved_rows(path: &Path) -> std::collections::BTreeSet<(String, String)> {
    db_rows(path, "MATCH (n:UnresolvedTarget) RETURN n.fqn, n.category")
        .into_iter()
        .map(|r| {
            (
                r.first().cloned().unwrap_or_default(),
                r.get(1).cloned().unwrap_or_default(),
            )
        })
        .collect()
}

/// The single `Scan` row of a DB file as `(sha, clean, key, at)`.
fn db_scan_row(path: &Path) -> (String, String, String, String) {
    let rows = db_rows(
        path,
        "MATCH (s:Scan) RETURN s.git_sha, s.git_clean, s.content_key, s.scanned_at",
    );
    assert_eq!(rows.len(), 1, "exactly one Scan row in {}", path.display());
    let r = &rows[0];
    (
        r.first().cloned().unwrap_or_default(),
        r.get(1).cloned().unwrap_or_default(),
        r.get(2).cloned().unwrap_or_default(),
        r.get(3).cloned().unwrap_or_default(),
    )
}

/// e2e tier -- real I/O: these tests drive `ingest`/`ingest_with_reuse`,
/// whose `ingest_records` spools to `std::env::temp_dir()` (two of them also
/// stage real temp dirs of their own). Each is `#[ignore]`d, so a plain
/// `cargo test` never runs one; the only entry point is the named guard
/// `cargo test-e2e` (= `cargo test tests::e2e:: -- --ignored`).
mod e2e {
    use super::*;

    #[test]
    #[ignore = "e2e tier: real I/O (temp spool dir); run via cargo test-e2e"]
    #[should_panic(expected = "FQN collision")]
    fn duplicate_fqn_panics() {
        let records = vec![
            Record::Module {
                fqn: "pkg".to_string(),
            },
            srec("n1", "pkg", "A", "/x/a.go"),
            srec("n2", "pkg", "A", "/x/b.go"),
        ];
        ingest(
            records,
            &IngestOptions {
                blacklist: &[],
                language: "go",
                config: None,
                base: None,
            },
        );
    }

    #[test]
    #[ignore = "e2e tier: real I/O (temp spool dir); run via cargo test-e2e"]
    fn module_shadowed_by_type_does_not_panic() {
        // Java permits a package `org.pkg.A` and a class `org.pkg.A` to coexist.
        // The type wins; the shadowed module is dropped and its Module→File edge
        // pruned (the File node stays, containing the units declared in it),
        // while unrelated modules, files, and edges survive.
        let records = vec![
            Record::Module {
                fqn: "org.pkg".to_string(),
            },
            Record::Module {
                fqn: "org.pkg.A".to_string(),
            },
            Record::Module {
                fqn: "org.pkg.A.deep".to_string(),
            },
            srec("n1", "org.pkg", "A", "/x/A.java"),
            srec("n2", "org.pkg.A.deep", "B", "/y/B.java"),
            file_rec("/x/A.java", "org.pkg", 30),
            file_rec("/y/B.java", "org.pkg.A.deep", 40),
            Record::Contains {
                from: "org.pkg".to_string(),
                to: "org.pkg.A".to_string(),
                properties: NodeProperties::default(),
            },
            Record::Contains {
                from: "org.pkg.A".to_string(),
                to: "org.pkg.A.deep".to_string(),
                properties: NodeProperties::default(),
            },
        ];
        let (graph, report) = ingest(
            records,
            &IngestOptions {
                blacklist: &[],
                language: "java",
                config: None,
                base: None,
            },
        );
        assert_eq!(report.shadowed_modules, 1);
        // PHASE_09: the module/type collision is at the ROOTED FQN — every
        // module is rooted under its `lang_switch` id (`java.`); the class
        // survives with its rooted canonical FQN.
        assert!(graph.nodes.contains_key("java.org.pkg.A"));
        assert_eq!(graph.nodes["java.org.pkg.A"].kind, NodeKind::Struct);
        // The parent package and the package nested under the shadowed name
        // survive; the shadowed package itself is not present.
        assert!(graph.nodes.contains_key("java.org.pkg"));
        assert!(graph.nodes.contains_key("java.org.pkg.A.deep"));
        assert!(graph.nodes.contains_key("java.org.pkg.A.deep.B"));
        // Files survive (their FQNs are absolute paths, never rooted) with
        // their own module·file·unit containment chains.
        assert!(graph.nodes.contains_key("/x/A.java"));
        assert!(graph.nodes.contains_key("/y/B.java"));
        assert_eq!(graph.nodes["/x/A.java"].kind, NodeKind::File);
        assert!(
            graph
                .contains
                .contains(&("java.org.pkg".to_string(), "/x/A.java".to_string()))
        );
        assert!(
            graph
                .contains
                .contains(&("/x/A.java".to_string(), "java.org.pkg.A".to_string()))
        );
        assert!(
            graph
                .contains
                .contains(&("java.org.pkg.A.deep".to_string(), "/y/B.java".to_string()))
        );
        assert!(
            graph
                .contains
                .contains(&("/y/B.java".to_string(), "java.org.pkg.A.deep.B".to_string()))
        );
        // But the shadowed package is not a parent: its Module→File edge and the
        // package chain through it are pruned.
        assert!(
            !graph
                .contains
                .contains(&("java.org.pkg.A".to_string(), "/x/A.java".to_string()))
        );
        assert!(!graph.contains.contains(&(
            "java.org.pkg.A".to_string(),
            "java.org.pkg.A.deep".to_string()
        )));
    }

    #[test]
    #[ignore = "e2e tier: real I/O (temp spool dir); run via cargo test-e2e"]
    fn function_shadowed_by_struct_does_not_panic() {
        // A class in a shadowed package (`p.A.test` in package `p.A`) renders
        // the same FQN as a method of the class `p.A`; the struct wins and the
        // function is dropped. Function-vs-function still panics.
        let records = vec![
            Record::Module {
                fqn: "p".to_string(),
            },
            Record::Module {
                fqn: "p.A".to_string(),
            },
            srec("n1", "p", "A", "/x/A.java"),
            srec("n2", "p.A", "test", "/y/test.java"),
            frec("n3", "p.A", "test", "/x/A.java"),
            frec("n5", "p.A", "other", "/x/A.java"),
            file_rec("/x/A.java", "p", 60),
            file_rec("/y/test.java", "p.A", 20),
            Record::Contains {
                from: "p".to_string(),
                to: "p.A".to_string(),
                properties: NodeProperties::default(),
            },
            Record::Contains {
                from: "n1".to_string(),
                to: "n3".to_string(),
                properties: NodeProperties::default(),
            },
            Record::Contains {
                from: "n1".to_string(),
                to: "n5".to_string(),
                properties: NodeProperties::default(),
            },
        ];
        let (graph, report) = ingest(
            records,
            &IngestOptions {
                blacklist: &[],
                language: "java",
                config: None,
                base: None,
            },
        );
        // The struct `p.A.test` (from the shadowed package) wins over the
        // method `p.A.test`; the distinct method `p.A.other` survives.
        // PHASE_09: rooting changes only the CROSS-language case — the
        // within-language shadow counts are unchanged and the collision is at
        // the rooted FQN (`java.`).
        assert_eq!(report.shadowed_functions, 1);
        assert_eq!(report.shadowed_modules, 1);
        assert!(graph.nodes.contains_key("java.p.A.test"));
        assert_eq!(graph.nodes["java.p.A.test"].kind, NodeKind::Struct);
        assert!(graph.nodes.contains_key("java.p.A.other"));
        // The shadowed module is gone as a module — `java.p.A` exists only as
        // the winning struct — and the file in it survives but loses its
        // module parent chain (`java.p→java.p.A` module edge pruned).
        assert_eq!(graph.nodes["java.p.A"].kind, NodeKind::Struct);
        assert!(graph.nodes.contains_key("/x/A.java"));
        assert!(graph.nodes.contains_key("/y/test.java"));
        assert!(
            graph
                .contains
                .contains(&("java.p".to_string(), "/x/A.java".to_string()))
        );
        assert!(
            !graph
                .contains
                .contains(&("java.p".to_string(), "java.p.A".to_string()))
        );
        assert!(
            graph
                .contains
                .contains(&("/x/A.java".to_string(), "java.p.A".to_string()))
        );
        assert!(
            graph
                .contains
                .contains(&("/y/test.java".to_string(), "java.p.A.test".to_string()))
        );
        // The dropped function's containment (by struct and by file) is pruned;
        // the surviving function's edges stay.
        assert!(
            !graph
                .contains
                .contains(&("java.p.A".to_string(), "java.p.A.test".to_string()))
        );
        assert!(
            !graph
                .contains
                .contains(&("/x/A.java".to_string(), "java.p.A.test".to_string()))
        );
        assert!(
            graph
                .contains
                .contains(&("java.p.A".to_string(), "java.p.A.other".to_string()))
        );
        assert!(
            graph
                .contains
                .contains(&("/x/A.java".to_string(), "java.p.A.other".to_string()))
        );
    }

    #[test]
    #[ignore = "e2e tier: real I/O (temp spool dir); run via cargo test-e2e"]
    fn module_replaces_unresolved_target() {
        // PHASE_09: rooting retires the original 'replaces' premise. A module's
        // identity is rooted (`java.tests`) while a foreign/unresolved name
        // stays verbatim (`tests`), so the module and the unresolved placeholder
        // no longer share an FQN — both survive, each with its own kind. (The
        // original behaviour, a real declaration superseding an
        // UnresolvedTarget placeholder at the SAME FQN, is still exercised by
        // `reuse_splice_reresolves_unresolved_edges_to_cached_real_nodes`.)
        let records = vec![
            Record::Unresolved {
                fqn: "tests".to_string(),
                category: Some("unknown".to_string()),
            },
            Record::Module {
                fqn: "tests".to_string(),
            },
        ];
        let (graph, _) = ingest(
            records,
            &IngestOptions {
                blacklist: &[],
                language: "java",
                config: None,
                base: None,
            },
        );
        // The module is rooted; the unresolved name is not.
        assert!(graph.nodes.contains_key("java.tests"));
        assert_eq!(graph.nodes["java.tests"].kind, NodeKind::Module);
        assert!(graph.nodes.contains_key("tests"));
        assert_eq!(graph.nodes["tests"].kind, NodeKind::UnresolvedTarget);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (temp spool dir); run via cargo test-e2e"]
    fn lang_switch_classifies_and_renders_per_record() {
        // A multi-language scan merges several frontend streams, each preceded
        // by a `lang_switch` record. code_type classification uses each
        // record's language (ts test rules vs go test rules), and Go `init`
        // disambiguation applies only to Go declarations.
        let records = vec![
            Record::LangSwitch {
                language: "go".to_string(),
            },
            Record::Module {
                fqn: "github.com/x/y".to_string(),
            },
            srec("g1", "github.com/x/y", "Store", "/abs/store.go"),
            Record::Function {
                id: "g2".to_string(),
                parent: "github.com/x/y".to_string(),
                name: "init".to_string(),
                params: vec![],
                file: "/abs/store.go".to_string(),
                path: "/abs/store.go".to_string(),
                start: 1,
                end: 5,
                start_line: 1,
                end_line: 5,
            },
            file_rec("/abs/store.go", "github.com/x/y", 100),
            Record::LangSwitch {
                language: "ts".to_string(),
            },
            Record::Module {
                fqn: "@co/ui".to_string(),
            },
            srec("t1", "@co/ui.src.app", "App", "/proj/src/app.ts"),
            Record::Function {
                id: "t2".to_string(),
                parent: "@co/ui.src.app".to_string(),
                name: "init".to_string(),
                params: vec![],
                file: "/proj/src/app.ts".to_string(),
                path: "/proj/src/app.ts".to_string(),
                start: 1,
                end: 5,
                start_line: 1,
                end_line: 5,
            },
            file_rec("/proj/src/app.ts", "@co/ui", 30),
            file_rec("/proj/src/app.test.ts", "@co/ui", 20),
            srec("t3", "@co/ui.src.app", "Helper", "/proj/src/app.test.ts"),
            // Two further languages reuse the SAME module identity `apg`:
            // rooting keeps them distinct (`rust.apg` vs `py.apg`).
            Record::LangSwitch {
                language: "rust".to_string(),
            },
            Record::Module {
                fqn: "apg".to_string(),
            },
            Record::LangSwitch {
                language: "py".to_string(),
            },
            Record::Module {
                fqn: "apg".to_string(),
            },
        ];
        let (graph, report) = ingest(
            records,
            &IngestOptions {
                blacklist: &[],
                language: "go",
                config: None,
                base: None,
            },
        );
        assert_eq!(report.skipped, 0);
        // PHASE_09: rooting makes a cross-language module-module FQN
        // collision impossible by construction, so nothing is shadowed.
        assert_eq!(
            report.shadowed_modules, 0,
            "rooting must keep rust.apg and py.apg distinct"
        );
        assert_eq!(report.shadowed_functions, 0);
        // Rooted module FQNs, one per language.
        for m in ["go.github.com/x/y", "ts.@co/ui", "rust.apg", "py.apg"] {
            assert_eq!(
                graph.nodes.get(m).map(|n| n.kind),
                Some(NodeKind::Module),
                "module `{m}` must be rooted under its language"
            );
        }
        // One Language node per `lang_switch` stream, bare id.
        for l in ["go", "ts", "rust", "py"] {
            assert_eq!(
                graph.nodes.get(l).map(|n| n.kind),
                Some(NodeKind::Language),
                "language root `{l}` must be materialised"
            );
        }
        // `Language -Contains-> Module` exactly once per module.
        for (lang, m) in [
            ("go", "go.github.com/x/y"),
            ("ts", "ts.@co/ui"),
            ("rust", "rust.apg"),
            ("py", "py.apg"),
        ] {
            assert!(
                graph.contains.contains(&(lang.to_string(), m.to_string())),
                "Language `{lang}` must contain `{m}`"
            );
        }
        // Rooted symbols inherit through `parent.name`; Go init is
        // file-disambiguated, the TS function named `init` is not.
        assert!(graph.nodes.contains_key("go.github.com/x/y.Store"));
        assert!(graph.nodes.contains_key("go.github.com/x/y.init#store.go"));
        assert!(graph.nodes.contains_key("ts.@co/ui.src.app.App"));
        assert!(graph.nodes.contains_key("ts.@co/ui.src.app.init"));
        // code_type is per-language: the Go store is src, the .test.ts file
        // (ts test rule) and its struct are test. File FQNs are paths.
        assert_eq!(graph.nodes["/abs/store.go"].code_type, "src");
        assert_eq!(graph.nodes["/proj/src/app.ts"].code_type, "src");
        assert_eq!(graph.nodes["/proj/src/app.test.ts"].code_type, "test");
        assert_eq!(graph.nodes["ts.@co/ui.src.app.Helper"].code_type, "test");
        // Module→File containment is rooted too.
        assert!(
            graph
                .contains
                .contains(&("go.github.com/x/y".to_string(), "/abs/store.go".to_string())),
            "the rooted module must contain its file"
        );
    }

    #[test]
    #[ignore = "e2e tier: real I/O (temp spool dir); run via cargo test-e2e"]
    fn scan_meta_record_becomes_scan_node() {
        // `apg scan` leads the stream with a scan_meta record (git state at
        // scan time); the ingestor turns it into the `scan/HEAD` Scan node.
        let records = vec![
            Record::ScanMeta {
                git_sha: Some("abc123".to_string()),
                git_clean: Some(true),
                content_key: Some("deadbeef".to_string()),
                scanned_at: "2026-09-07T00:00:00Z".to_string(),
            },
            Record::Module {
                fqn: "github.com/x/y".to_string(),
            },
        ];
        let (graph, _) = ingest(
            records,
            &IngestOptions {
                blacklist: &[],
                language: "go",
                config: None,
                base: None,
            },
        );
        let n = &graph.nodes[SCAN_HEAD];
        assert_eq!(n.kind, NodeKind::Scan);
        assert_eq!(n.git_sha.as_deref(), Some("abc123"));
        assert_eq!(n.git_clean, Some(true));
        assert_eq!(
            n.content_key.as_deref(),
            Some("deadbeef"),
            "the stream's content-identity key must reach the DB Scan node"
        );
        assert_eq!(n.scanned_at.as_deref(), Some("2026-09-07T00:00:00Z"));

        // A non-git scan emits a scan_meta with no git fields; the node still
        // records the timestamp.
        let (graph, _) = ingest(
            vec![Record::ScanMeta {
                git_sha: None,
                git_clean: None,
                content_key: None,
                scanned_at: "2026-09-07T00:00:00Z".to_string(),
            }],
            &IngestOptions {
                blacklist: &[],
                language: "go",
                config: None,
                base: None,
            },
        );
        let n = &graph.nodes[SCAN_HEAD];
        assert_eq!(n.kind, NodeKind::Scan);
        assert_eq!(n.git_sha, None);
        assert_eq!(n.git_clean, None);
        assert_eq!(n.content_key, None);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (temp spool dir); run via cargo test-e2e"]
    fn end_to_end_ingest_resolves_edges() {
        let records = vec![
            Record::Module {
                fqn: "github.com/x/y".to_string(),
            },
            srec("n1", "github.com/x/y", "Store", "/abs/store.go"),
            frec("n2", "github.com/x/y", "Compute", "/abs/store.go"),
            Record::Function {
                id: "n2b".to_string(),
                parent: "github.com/x/y.Store".to_string(),
                name: "Get".to_string(),
                params: vec![],
                file: "/abs/store.go".to_string(),
                path: "/abs/store.go".to_string(),
                start: 1,
                end: 50,
                start_line: 1,
                end_line: 50,
            },
            file_rec("/abs/store.go", "github.com/x/y", 100),
            Record::Unresolved {
                fqn: "fmt.Errorf".to_string(),
                category: Some("stdlib".to_string()),
            },
            Record::Contains {
                from: "n1".to_string(),
                to: "n2b".to_string(),
                properties: NodeProperties::default(),
            },
            Record::UnresolvedCall {
                from: "n2".to_string(),
                to: "fmt.Errorf".to_string(),
                target_type: String::new(),
            },
        ];
        let (graph, report) = ingest(
            records,
            &IngestOptions {
                blacklist: &[],
                language: "go",
                config: None,
                base: None,
            },
        );
        assert_eq!(report.skipped, 0);
        // PHASE_09: every module/symbol FQN is rooted under the scan's
        // `lang_switch` id (`go.`); the unresolved target stays verbatim.
        assert!(graph.nodes.contains_key("go.github.com/x/y.Store"));
        assert!(graph.nodes.contains_key("go.github.com/x/y.Compute"));
        assert!(graph.nodes.contains_key("go.github.com/x/y.Store.Get"));
        assert!(graph.nodes.contains_key("fmt.Errorf"));
        // File layer: module contains the file, the file contains its units,
        // and methods stay under their struct.
        assert!(
            graph
                .contains
                .contains(&("go.github.com/x/y".to_string(), "/abs/store.go".to_string()))
        );
        assert!(graph.contains.contains(&(
            "/abs/store.go".to_string(),
            "go.github.com/x/y.Store".to_string()
        )));
        assert!(graph.contains.contains(&(
            "/abs/store.go".to_string(),
            "go.github.com/x/y.Compute".to_string()
        )));
        assert!(!graph.contains.contains(&(
            "go.github.com/x/y".to_string(),
            "go.github.com/x/y.Store".to_string()
        )));
        assert!(graph.contains.contains(&(
            "go.github.com/x/y.Store".to_string(),
            "go.github.com/x/y.Store.Get".to_string()
        )));
        assert!(graph.unresolved_calls.contains(&(
            "go.github.com/x/y.Compute".to_string(),
            "fmt.Errorf".to_string(),
            String::new()
        )));
    }

    #[test]
    #[ignore = "e2e tier: real I/O (temp spool dir); run via cargo test-e2e"]
    fn planned_node_lands_with_parent_contains() {
        // A plan-side planned_node record lands as an Implementation node with
        // `status: planned` (no location); a `parent` names the containing node
        // via a Contains edge (a valid File→Struct pair).
        let records = vec![
            Record::PlannedNode {
                fqn: "/abs/gateway.go".to_string(),
                kind: "file".to_string(),
                name: "gateway.go".to_string(),
                parent: String::new(),
            },
            Record::PlannedNode {
                fqn: "github.com/x/gateway".to_string(),
                kind: "struct".to_string(),
                name: "Gateway".to_string(),
                parent: "/abs/gateway.go".to_string(),
            },
        ];
        let (graph, _) = ingest(
            records,
            &IngestOptions {
                blacklist: &[],
                language: "go",
                config: None,
                base: None,
            },
        );
        // The planned node lands as a Struct with status=planned (no location).
        let fqn = "github.com/x/gateway".to_string();
        assert_eq!(graph.nodes[&fqn].kind, NodeKind::Struct);
        assert_eq!(graph.nodes[&fqn].status.as_deref(), Some("planned"));
        assert!(graph.nodes[&fqn].location.is_none());
        assert_eq!(
            graph.nodes["/abs/gateway.go"].status.as_deref(),
            Some("planned")
        );
        // The planned File→Struct containment lands (a valid Contains pair).
        assert!(
            graph
                .contains
                .contains(&("/abs/gateway.go".to_string(), fqn))
        );
    }

    #[test]
    #[ignore = "e2e tier: real I/O (temp spool dir); run via cargo test-e2e"]
    fn scanner_replace_supersedes_planned_node_and_keeps_edges() {
        // The scanner-replace (PlanExecution-SPEC.md): a real declaration at a
        // planned FQN supersedes the planned node (status cleared, location
        // filled), and FQN-keyed incident edges (implemented-by) re-point to
        // the real node automatically — the why-to-code chain resolves to real
        // code.
        let records = vec![
            Record::Module {
                fqn: "github.com/x/y".to_string(),
            },
            // The scanner's real declaration of the planned FQN.
            srec("n1", "github.com/x/y", "Gateway", "/abs/gateway.go"),
            file_rec("/abs/gateway.go", "github.com/x/y", 50),
            // A solution component the planned node is implemented-by.
            Record::Component {
                fqn: "solution.component.checkout".to_string(),
                name: "checkout".to_string(),
                body: String::new(),
                properties: NodeProperties::default(),
            },
            Record::SpecImplementedBy {
                from: "solution.component.checkout".to_string(),
                to: "go.github.com/x/y.Gateway".to_string(),
                properties: NodeProperties::default(),
            },
            Record::PlannedNode {
                fqn: "go.github.com/x/y.Gateway".to_string(),
                kind: "struct".to_string(),
                name: "Gateway".to_string(),
                parent: String::new(),
            },
        ];
        let (graph, _) = ingest(
            records,
            &IngestOptions {
                blacklist: &[],
                language: "go",
                config: None,
                base: None,
            },
        );
        let fqn = "go.github.com/x/y.Gateway".to_string();
        let node = &graph.nodes[&fqn];
        // The real (scanned) node won: present, located, not planned.
        assert_eq!(node.kind, NodeKind::Struct);
        assert!(node.status.is_none(), "scanner-replace clears status");
        assert!(node.location.is_some(), "real node carries its location");
        // The implemented-by edge re-points to the realized node (FQN-keyed).
        assert!(
            graph
                .spec_implemented_by
                .contains(&("solution.component.checkout".to_string(), fqn.clone()))
        );
        // The real File→Struct containment landed from the scanner.
        assert!(
            graph
                .contains
                .contains(&("/abs/gateway.go".to_string(), fqn))
        );
    }

    #[test]
    #[ignore = "e2e tier: real I/O (temp spool dir); run via cargo test-e2e"]
    fn blacklisted_nodes_and_edges_dropped() {
        let records = vec![
            Record::Module {
                fqn: "keep.mod".to_string(),
            },
            Record::Module {
                fqn: "drop.mod".to_string(),
            },
            srec("n1", "keep.mod", "A", "/x/a.go"),
            srec("n2", "drop.mod", "B", "/x/b.go"),
            file_rec("/x/a.go", "keep.mod", 10),
            file_rec("/x/b.go", "drop.mod", 10),
            Record::Contains {
                from: "drop.mod".to_string(),
                to: "/x/b.go".to_string(),
                properties: NodeProperties::default(),
            },
        ];
        let (graph, report) = ingest(
            records,
            &IngestOptions {
                blacklist: &["go.drop.mod".to_string()],
                language: "go",
                config: None,
                base: None,
            },
        );
        assert!(report.skipped >= 3);
        // PHASE_09: every module FQN is rooted (`go.`), and
        // `is_blacklisted` matches the ROOTED FQN — hence the rooted pattern.
        assert!(graph.nodes.contains_key("go.keep.mod"));
        assert!(!graph.nodes.contains_key("go.drop.mod"));
        assert!(!graph.nodes.contains_key("go.drop.mod.B"));
        // A file whose parent module is blacklisted is dropped along with its
        // units; the surviving file keeps its module and unit edges.
        assert!(!graph.nodes.contains_key("/x/b.go"));
        assert!(graph.nodes.contains_key("/x/a.go"));
        assert!(
            graph
                .contains
                .contains(&("go.keep.mod".to_string(), "/x/a.go".to_string()))
        );
        assert!(
            graph
                .contains
                .contains(&("/x/a.go".to_string(), "go.keep.mod.A".to_string()))
        );
        assert!(!graph.contains.is_empty());
    }

    #[test]
    #[ignore = "e2e tier: real I/O (temp spool dir); run via cargo test-e2e"]
    fn cached_cross_file_edges_survive_unit_order() {
        // Regression: the win-B fact splice merges ALL cached nodes before ANY
        // cached edges, so a cross-file `calls`/`uses` edge whose target unit is
        // visited later is not dropped. The reuse list is deliberately ordered
        // so the depending file comes FIRST (its callee lands in a later unit).
        use apg::cache::{CacheKey, FactStore, FileFragment, ScanConfigKey};
        use apg::graph::{Graph, Location, Node, NodeKind};
        use std::path::PathBuf;

        let dir = std::env::temp_dir().join(format!("apg-splice-order-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let cache_key = CacheKey::compute(&ScanConfigKey::default());
        let mut store = FactStore::at(dir.join("facts"));

        // Build a two-file graph: b/b.go.Later calls a/a.go.Leaf.
        let mut g = Graph::default();
        g.nodes.insert(
            "scratch".to_string(),
            Node {
                kind: NodeKind::Module,
                ..Node::default()
            },
        );
        for (fqn, path, kind) in [
            ("scratch/a.Leaf", "/w/a/a.go", NodeKind::Function),
            ("scratch/b.Later", "/w/b/b.go", NodeKind::Function),
        ] {
            g.nodes.insert(
                fqn.to_string(),
                Node {
                    kind,
                    location: Some(Location {
                        path: PathBuf::from(path),
                        start: 0,
                        end: 1,
                        start_line: 1,
                        end_line: 1,
                    }),
                    code_type: "src".into(),
                    ..Node::default()
                },
            );
        }
        for path in ["/w/a/a.go", "/w/b/b.go"] {
            g.nodes.insert(
                path.to_string(),
                Node {
                    kind: NodeKind::File,
                    location: Some(Location {
                        path: PathBuf::from(path),
                        start: 0,
                        end: 0,
                        start_line: 1,
                        end_line: 1,
                    }),
                    ..Node::default()
                },
            );
            g.contains.insert(("scratch".to_string(), path.to_string()));
        }
        g.contains
            .insert(("/w/a/a.go".to_string(), "scratch/a.Leaf".to_string()));
        g.contains
            .insert(("/w/b/b.go".to_string(), "scratch/b.Later".to_string()));
        g.calls
            .insert(("scratch/b.Later".to_string(), "scratch/a.Leaf".to_string()));

        for (abs, rel) in [("/w/a/a.go", "a/a.go"), ("/w/b/b.go", "b/b.go")] {
            let frag = FileFragment::from_graph(&g, abs, rel, &format!("oid-{rel}"), "go");
            store.put(&frag, "/w", &cache_key).unwrap();
        }

        // The reuse list puts b/b.go (the caller) BEFORE a/a.go (the callee).
        let reuse = Reuse {
            store: &store,
            cache_key: &cache_key,
            files: vec![
                (
                    "b/b.go".to_string(),
                    "go".to_string(),
                    "oid-b/b.go".to_string(),
                ),
                (
                    "a/a.go".to_string(),
                    "go".to_string(),
                    "oid-a/a.go".to_string(),
                ),
            ],
            reader_root: "/fresh".to_string(),
            skipped_langs: BTreeSet::new(),
        };

        let (graph, _) = ingest_with_reuse(
            Vec::<Record>::new(),
            &IngestOptions {
                blacklist: &[],
                language: "go",
                config: None,
                base: None,
            },
            Some(&reuse),
        );
        // All nodes landed and the cross-file call survived (it would be lost if
        // edges were merged per-unit before every node existed).
        assert!(graph.nodes.contains_key("b/b.go"), "{:?}", graph.nodes);
        assert!(graph.nodes.contains_key("a/a.go"));
        assert!(graph.nodes.contains_key("scratch/b.Later"));
        assert!(graph.nodes.contains_key("scratch/a.Leaf"));
        assert!(
            graph
                .calls
                .contains(&("scratch/b.Later".to_string(), "scratch/a.Leaf".to_string())),
            "the cached cross-file call must survive unit order: {:?}",
            graph.calls
        );
        assert!(
            graph
                .contains
                .contains(&("scratch".to_string(), "b/b.go".to_string()))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Phase-04 task-24: the reuse splice re-resolves unresolved edges whose
    /// target FQN a cached unit declares as a real node. Pass 1 replaces the
    /// UnresolvedTarget placeholder, but the spool-authored unresolved edges
    /// still name the FQN; pass 4 converts them to `calls`/`uses` and GCs the
    /// now-unreferenced row, so the assembled graph equals a full-scan assembly
    /// of the same tree — the falsifiable re-resolution claim (task-16).
    #[test]
    #[ignore = "e2e tier: real I/O (temp spool dir); run via cargo test-e2e"]
    fn reuse_splice_reresolves_unresolved_edges_to_cached_real_nodes() {
        use apg::cache::{CacheKey, FactStore, FileFragment, ScanConfigKey};
        use apg::graph::{Graph, Location, Node, NodeKind};
        use std::path::PathBuf;

        let dir = std::env::temp_dir().join(format!("apg-reresolve-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let cache_key = CacheKey::compute(&ScanConfigKey::default());
        let mut store = FactStore::at(dir.join("facts"));

        // The cached unit b/b.go declares the two real targets.
        let mut cached = Graph::default();
        cached.nodes.insert(
            "go.scratch".to_string(),
            Node {
                kind: NodeKind::Module,
                ..Node::default()
            },
        );
        cached.nodes.insert(
            "b.go".to_string(),
            Node {
                kind: NodeKind::File,
                location: Some(Location {
                    path: PathBuf::from("b.go"),
                    start: 0,
                    end: 0,
                    start_line: 1,
                    end_line: 9,
                }),
                ..Node::default()
            },
        );
        for (fqn, kind, start_line) in [
            ("go.scratch.Callee", NodeKind::Function, 2u32),
            ("go.scratch.Model", NodeKind::Struct, 6u32),
        ] {
            cached.nodes.insert(
                fqn.to_string(),
                Node {
                    kind,
                    location: Some(Location {
                        path: PathBuf::from("b.go"),
                        start: 0,
                        end: 1,
                        start_line,
                        end_line: start_line,
                    }),
                    code_type: "src".into(),
                    ..Node::default()
                },
            );
        }
        cached
            .contains
            .insert(("go.scratch".to_string(), "b.go".to_string()));
        cached
            .contains
            .insert(("b.go".to_string(), "go.scratch.Callee".to_string()));
        cached
            .contains
            .insert(("b.go".to_string(), "go.scratch.Model".to_string()));
        let frag = FileFragment::from_graph(&cached, "b.go", "b.go", "oid-b", "go");
        store.put(&frag, "/fresh", &cache_key).unwrap();

        // The freshly re-emitted spool: a.go's Caller authors unresolved edges
        // at BOTH cached real FQNs plus one genuinely-unresolved target.
        let spool = vec![
            Record::Module {
                fqn: "scratch".to_string(),
            },
            Record::Function {
                id: "s1".to_string(),
                parent: "scratch".to_string(),
                name: "Caller".to_string(),
                params: vec![],
                file: "a.go".to_string(),
                path: "a.go".to_string(),
                start: 0,
                end: 1,
                start_line: 1,
                end_line: 1,
            },
            file_rec("a.go", "scratch", 10),
            Record::Unresolved {
                fqn: "go.scratch.Callee".to_string(),
                category: Some("unknown".to_string()),
            },
            Record::Unresolved {
                fqn: "go.scratch.Model".to_string(),
                category: Some("external".to_string()),
            },
            Record::Unresolved {
                fqn: "ghost.External".to_string(),
                category: Some("external".to_string()),
            },
            Record::UnresolvedCall {
                from: "s1".to_string(),
                to: "go.scratch.Callee".to_string(),
                target_type: "func()".to_string(),
            },
            Record::UnresolvedUse {
                from: "s1".to_string(),
                to: "go.scratch.Model".to_string(),
            },
            Record::UnresolvedCall {
                from: "s1".to_string(),
                to: "ghost.External".to_string(),
                target_type: String::new(),
            },
        ];
        let reuse = Reuse {
            store: &store,
            cache_key: &cache_key,
            files: vec![("b.go".to_string(), "go".to_string(), "oid-b".to_string())],
            reader_root: "/fresh".to_string(),
            skipped_langs: BTreeSet::new(),
        };
        let (assembled, _) = ingest_with_reuse(
            spool,
            &IngestOptions {
                blacklist: &[],
                language: "go",
                config: None,
                base: None,
            },
            Some(&reuse),
        );

        // The same tree resolved from scratch: the cached targets are declared
        // here and the call/use are RESOLVED edges.
        let full = vec![
            Record::Module {
                fqn: "scratch".to_string(),
            },
            Record::Function {
                id: "f1".to_string(),
                parent: "scratch".to_string(),
                name: "Caller".to_string(),
                params: vec![],
                file: "a.go".to_string(),
                path: "a.go".to_string(),
                start: 0,
                end: 1,
                start_line: 1,
                end_line: 1,
            },
            Record::Function {
                id: "f2".to_string(),
                parent: "scratch".to_string(),
                name: "Callee".to_string(),
                params: vec![],
                file: "b.go".to_string(),
                path: "b.go".to_string(),
                start: 0,
                end: 1,
                start_line: 2,
                end_line: 2,
            },
            srec("f3", "scratch", "Model", "b.go"),
            file_rec("a.go", "scratch", 10),
            file_rec("b.go", "scratch", 9),
            Record::Unresolved {
                fqn: "ghost.External".to_string(),
                category: Some("external".to_string()),
            },
            Record::Calls {
                from: "f1".to_string(),
                to: "f2".to_string(),
                properties: NodeProperties::default(),
            },
            Record::Uses {
                from: "f1".to_string(),
                to: "f3".to_string(),
                properties: NodeProperties::default(),
            },
            Record::UnresolvedCall {
                from: "f1".to_string(),
                to: "ghost.External".to_string(),
                target_type: String::new(),
            },
        ];
        let (reference, _) = ingest(
            full,
            &IngestOptions {
                blacklist: &[],
                language: "go",
                config: None,
                base: None,
            },
        );

        // (a) the converted edges appear in `calls`/`uses`.
        assert!(
            assembled.calls.contains(&(
                "go.scratch.Caller".to_string(),
                "go.scratch.Callee".to_string()
            )),
            "the unresolved call to a cached real Function must move to calls: {:?}",
            assembled.calls
        );
        assert!(
            assembled.uses.contains(&(
                "go.scratch.Caller".to_string(),
                "go.scratch.Model".to_string()
            )),
            "the unresolved use of a cached real Struct must move to uses: {:?}",
            assembled.uses
        );
        // (b) NO unresolved edge targets a real (non-UnresolvedTarget) node.
        for (from, to, _) in &assembled.unresolved_calls {
            assert!(
                assembled
                    .nodes
                    .get(to)
                    .is_some_and(|n| n.kind == NodeKind::UnresolvedTarget),
                "unresolved_call {from} -> {to} must not target a real project FQN"
            );
        }
        for (from, to) in &assembled.unresolved_uses {
            assert!(
                assembled
                    .nodes
                    .get(to)
                    .is_some_and(|n| n.kind == NodeKind::UnresolvedTarget),
                "unresolved_use {from} -> {to} must not target a real project FQN"
            );
        }
        // (c) the genuine unresolved edge + exactly ONE UnresolvedTarget row.
        assert!(assembled.unresolved_calls.contains(&(
            "go.scratch.Caller".to_string(),
            "ghost.External".to_string(),
            String::new()
        )));
        let unresolved = |g: &Graph| -> BTreeSet<String> {
            g.nodes
                .iter()
                .filter(|(_, n)| n.kind == NodeKind::UnresolvedTarget)
                .map(|(fqn, _)| fqn.clone())
                .collect()
        };
        assert_eq!(
            unresolved(&assembled),
            BTreeSet::from(["ghost.External".to_string()]),
            "exactly one shared UnresolvedTarget row may survive"
        );
        // (d) the assembled node/edge/unresolved sets equal a full-scan assembly.
        let node_set = |g: &Graph| -> BTreeSet<String> { g.nodes.keys().cloned().collect() };
        assert_eq!(node_set(&assembled), node_set(&reference));
        assert_eq!(assembled.contains, reference.contains);
        assert_eq!(assembled.calls, reference.calls);
        assert_eq!(assembled.uses, reference.uses);
        assert_eq!(assembled.unresolved_calls, reference.unresolved_calls);
        assert_eq!(assembled.unresolved_uses, reference.unresolved_uses);
        assert_eq!(unresolved(&assembled), unresolved(&reference));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// feedback-102: a language whose frontend was skipped re-emits nothing, so
    /// its global module scaffolding (pure-intermediate modules, file-less
    /// descendants, and every `Module -> Module` edge) must be replayed from the
    /// store into the assembled graph — the `graph.jsonl` export source — or the
    /// export is structurally incomplete even though the spliced DB keeps the
    /// seed's rows. The replay is gated on the exact `skipped_langs` verdict, so
    /// a spawned language is never shadowed by stale cache rows.
    #[test]
    #[ignore = "e2e tier: real I/O (temp spool dir); run via cargo test-e2e"]
    fn skipped_language_scaffolding_is_replayed_into_the_assembly() {
        use apg::cache::{CacheKey, FactStore, FileFragment, ModuleScaffolding, ScanConfigKey};
        use apg::graph::{Graph, Location, Node, NodeKind};
        use std::path::PathBuf;

        let dir = std::env::temp_dir().join(format!("apg-splice-scaffold-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let cache_key = CacheKey::compute(&ScanConfigKey::default());
        let mut store = FactStore::at(dir.join("facts"));

        let module = || Node {
            kind: NodeKind::Module,
            ..Node::default()
        };
        let located = |kind: NodeKind, path: &str| Node {
            kind,
            location: Some(Location {
                path: PathBuf::from(path),
                start: 0,
                end: 1,
                start_line: 1,
                end_line: 1,
            }),
            code_type: "src".into(),
            ..Node::default()
        };
        let skipped = "csharp/T.cs";
        let mut prev = Graph::default();
        prev.nodes.insert("Apg".into(), module());
        prev.nodes.insert("Apg.CsharpFrontend".into(), module());
        prev.nodes
            .insert("Apg.CsharpFrontend.Tests".into(), module());
        prev.nodes
            .insert("Apg.CsharpFrontend.Tests.Inline".into(), module());
        prev.nodes
            .insert(skipped.into(), located(NodeKind::File, skipped));
        prev.nodes.insert(
            "Apg.CsharpFrontend.Tests.Program".into(),
            located(NodeKind::Struct, skipped),
        );
        prev.contains
            .insert(("Apg".into(), "Apg.CsharpFrontend".into()));
        prev.contains.insert((
            "Apg.CsharpFrontend".into(),
            "Apg.CsharpFrontend.Tests".into(),
        ));
        prev.contains.insert((
            "Apg.CsharpFrontend.Tests".into(),
            "Apg.CsharpFrontend.Tests.Inline".into(),
        ));
        prev.contains
            .insert(("Apg.CsharpFrontend.Tests".into(), skipped.into()));
        prev.contains
            .insert((skipped.into(), "Apg.CsharpFrontend.Tests.Program".into()));

        let frag = FileFragment::from_graph(&prev, skipped, "csharp/T.cs", "oid-t", "csharp");
        store.put(&frag, "/x", &cache_key).unwrap();
        let scaffolding = ModuleScaffolding::extract(&prev, std::path::Path::new("/x"));
        store.put_scaffolding_all(&scaffolding, &cache_key).unwrap();

        // The whole assembly comes from the cache (the changed language
        // contributes no records in this unit test).
        let reuse = Reuse {
            store: &store,
            cache_key: &cache_key,
            files: vec![("csharp/T.cs".into(), "csharp".into(), "oid-t".into())],
            reader_root: "/x".into(),
            skipped_langs: ["csharp".into()].into_iter().collect(),
        };
        let (graph, _) = ingest_with_reuse(
            Vec::<Record>::new(),
            &IngestOptions {
                blacklist: &[],
                language: "csharp",
                config: None,
                base: None,
            },
            Some(&reuse),
        );
        for m in [
            "Apg",
            "Apg.CsharpFrontend",
            "Apg.CsharpFrontend.Tests",
            "Apg.CsharpFrontend.Tests.Inline",
        ] {
            assert!(
                graph.nodes.contains_key(m),
                "the skipped language's module `{m}` must be replayed: {:?}",
                graph.nodes.keys().collect::<Vec<_>>()
            );
        }
        for (from, to) in [
            ("Apg", "Apg.CsharpFrontend"),
            ("Apg.CsharpFrontend", "Apg.CsharpFrontend.Tests"),
            (
                "Apg.CsharpFrontend.Tests",
                "Apg.CsharpFrontend.Tests.Inline",
            ),
        ] {
            assert!(
                graph.contains.contains(&(from.to_string(), to.to_string())),
                "the `Module -> Module` edge {from} -> {to} must be replayed"
            );
        }
        // The reused file's own unit and its Module→File edge survive.
        assert!(graph.nodes.contains_key(skipped));
        assert!(
            graph
                .contains
                .contains(&("Apg.CsharpFrontend.Tests".to_string(), skipped.to_string()))
        );

        // The replay is gated on the skipped-language verdict: with an empty
        // `skipped_langs` the same store contributes no scaffolding.
        let not_skipped = Reuse {
            store: &store,
            cache_key: &cache_key,
            files: vec![("csharp/T.cs".into(), "csharp".into(), "oid-t".into())],
            reader_root: "/x".into(),
            skipped_langs: BTreeSet::new(),
        };
        let (bare, _) = ingest_with_reuse(
            Vec::<Record>::new(),
            &IngestOptions {
                blacklist: &[],
                language: "csharp",
                config: None,
                base: None,
            },
            Some(&not_skipped),
        );
        assert!(
            !bare.nodes.contains_key("Apg"),
            "a language that was not skipped must not replay cached scaffolding"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Phase-03 task-9 — the splice path END-TO-END: a scratch /tmp git repo
    /// driven by the CANDIDATE binary only
    /// (global.constraint.no-real-project-test). (1) The DETERMINISTIC
    /// no-full-DB-rebuild observable: the distinct splice verdict line is
    /// PRESENT on a localized re-scan and ABSENT on a full rebuild, whose path
    /// emits the full-load lines instead — a named observable, not the mere
    /// absence of a frontend spawn. (2) The ENUMERATED equivalence oracle:
    /// spliced vs full rebuild on per-label and per-rel-type counts, the
    /// UnresolvedTarget set by FQN WITH categories (per-category counts), the
    /// `Scan` row, a spine query, and the EXPORT (`graph.jsonl` equality plus a
    /// JSONL → Graph → JSONL round-trip).
    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo/spawned apg/db.lbug); run via cargo test-e2e"]
    fn splice_path_is_taken_and_equals_a_full_rebuild() {
        use apg::testutil::{ApgCommand, Repo};

        let repo = Repo::new("p3-splice-e2e");
        // The isolated HOME lives OUTSIDE the scanned repo. Every scan runs
        // the Go frontend, and the `go` toolchain writes its telemetry (and
        // may write other caches) under HOME; with HOME inside `repo.root`
        // those files are new, untracked members of the scanned tree, so a
        // later scan treats them as changed `misc` targets — spawning a
        // FILTERED misc stream and skipping its scaffolding replay, which
        // makes the splice assembly diverge from a full rebuild. Keeping
        // HOME outside keeps every scan looking at the same tree.
        let home = repo
            .root
            .parent()
            .expect("the repo has a parent scratch dir")
            .join(format!(
                "{}-home-{}",
                repo.root
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                std::process::id()
            ));
        std::fs::create_dir_all(&home).unwrap();
        // A real Go module: `b` depends on `a`, `c` depends on `b`.
        repo.write("go.mod", "module scratch\n\ngo 1.21\n");
        repo.write(
            "a/a.go",
            "package a\n\n// A is a struct.\ntype A struct {\n\tX int\n}\n\n// Leaf is the leaf function.\nfunc Leaf() int { return 1 }\n",
        );
        repo.write(
            "b/b.go",
            "package b\n\nimport \"scratch/a\"\n\n// B is a struct.\ntype B struct {\n\tA a.A\n}\n\n// Foo calls the leaf.\nfunc Foo() int { return a.Leaf() }\n",
        );
        repo.write(
            "c/c.go",
            "package c\n\nimport \"scratch/b\"\n\n// Bar calls Foo.\nfunc Bar() int { return b.Foo() }\n",
        );
        repo.commit_all("source");
        let run = |args: &[&str]| {
            ApgCommand::new(args)
                .cwd(&repo.root)
                .env("HOME", home.to_str().unwrap())
                .output()
        };
        let init = run(&["init", "."]);
        assert!(
            init.status.success(),
            "init: {}",
            String::from_utf8_lossy(&init.stderr)
        );
        repo.commit_all("apg init");

        let trans = repo.root.join(apg::specs::LAYOUT).join(apg::specs::TRANS);
        let db_path = trans.join("db.lbug");
        let jsonl_path = trans.join("graph.jsonl");

        // The cold scan has no previous DB: a FULL load, with no splice verdict.
        let cold = run(&["scan", "."]);
        let cold_err = String::from_utf8_lossy(&cold.stderr).into_owned();
        assert!(cold.status.success(), "cold scan: {cold_err}");
        assert!(
            cold_err.contains("[load] writing parquet load files"),
            "the cold scan must full-load: {cold_err}"
        );
        assert!(
            !cold_err.contains("[load] splice:"),
            "the cold scan must not splice: {cold_err}"
        );
        // The cold scan's own Scan row — the seed the splice must REFRESH.
        let cold_scan = db_scan_row(&db_path);

        // A localized BODY-ONLY edit to the leaf file: `a.go` changes, b/c do not.
        repo.write(
            "a/a.go",
            "package a\n\n// A is a struct.\ntype A struct {\n\tX int\n}\n\n// Leaf is the leaf function.\nfunc Leaf() int { return 42 }\n",
        );

        let inc = run(&["scan", "."]);
        let inc_err = String::from_utf8_lossy(&inc.stderr).into_owned();
        assert!(inc.status.success(), "incremental scan: {inc_err}");
        // (1) The distinct splice verdict is PRESENT and the full-load lines are
        // ABSENT — the splice path was genuinely taken (a full rebuild cannot
        // produce this line).
        assert!(
            inc_err.contains("[load] splice:"),
            "the localized re-scan must take the splice path: {inc_err}"
        );
        assert!(
            !inc_err.contains("[load] writing parquet load files"),
            "the splice path must skip the full load: {inc_err}"
        );
        assert!(
            inc_err.contains("upserted") && inc_err.contains("full load skipped"),
            "the splice verdict must name the applied delta and the skipped full load: {inc_err}"
        );

        // Snapshot the spliced pair, then FORCE a full rebuild of the SAME tree:
        // clear the DB, the export AND the shared fact cache so neither the
        // fast-path nor win-B reuse can engage.
        //
        // The snapshots live OUTSIDE the scanned tree: the bundled structural
        // scanner graphs every untracked, non-gitignored file it walks, so a
        // `spliced.lbug`/`spliced.jsonl` written inside `repo.root` would be
        // graphed by the full rebuild but not by the splice snapshot that
        // predates it — the oracle would then compare two different trees.
        // The parent scratch dir is outside the scan root, so both scans see
        // exactly the same tree.
        let snapshot_dir = repo
            .root
            .parent()
            .expect("the repo has a parent scratch dir")
            .join(format!(
                "{}-splice-snapshot-{}",
                repo.root
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                std::process::id()
            ));
        std::fs::create_dir_all(&snapshot_dir).unwrap();
        let spliced_db = snapshot_dir.join("spliced.lbug");
        let spliced_jsonl = snapshot_dir.join("spliced.jsonl");
        std::fs::copy(&db_path, &spliced_db).unwrap();
        std::fs::copy(&jsonl_path, &spliced_jsonl).unwrap();
        std::fs::remove_file(&db_path).unwrap();
        std::fs::remove_file(&jsonl_path).unwrap();
        let _ = std::fs::remove_dir_all(repo.root.join(".git/apg/facts"));
        let full = run(&["scan", "."]);
        let full_err = String::from_utf8_lossy(&full.stderr).into_owned();
        assert!(full.status.success(), "full rebuild: {full_err}");
        assert!(
            full_err.contains("[load] writing parquet load files"),
            "the rebuild must full-load: {full_err}"
        );
        assert!(
            !full_err.contains("[load] splice:"),
            "a full rebuild must not emit the splice verdict: {full_err}"
        );

        // (2) The enumerated equivalence oracle: spliced vs full rebuild.
        let spliced_counts = db_table_counts(&spliced_db);
        let full_counts = db_table_counts(&db_path);
        assert_eq!(
            spliced_counts, full_counts,
            "every table's count must equal a full rebuild"
        );
        for label in ["Module", "File", "Struct", "Function", "UnresolvedTarget"] {
            assert!(
                spliced_counts.contains_key(label),
                "the oracle must enumerate the {label} node table: {spliced_counts:?}"
            );
        }
        for table in [
            "Contains",
            "Calls",
            "Uses",
            "UnresolvedCall",
            "UnresolvedUse",
        ] {
            assert!(
                spliced_counts.contains_key(table),
                "the oracle must enumerate the {table} rel table: {spliced_counts:?}"
            );
            assert_eq!(
                spliced_counts.get(table),
                full_counts.get(table),
                "{table} count must equal a full rebuild"
            );
        }

        let spliced_unres = db_unresolved_rows(&spliced_db);
        let full_unres = db_unresolved_rows(&db_path);
        assert_eq!(
            spliced_unres, full_unres,
            "the UnresolvedTarget set by (fqn, category) must equal a full rebuild's"
        );
        let per_category = |rows: &std::collections::BTreeSet<(String, String)>| {
            let mut m: std::collections::BTreeMap<String, usize> =
                std::collections::BTreeMap::new();
            for (_, c) in rows {
                *m.entry(c.clone()).or_default() += 1;
            }
            m
        };
        assert_eq!(
            per_category(&spliced_unres),
            per_category(&full_unres),
            "the per-category unresolved counts must equal a full rebuild's"
        );

        // The Scan row: REFRESHED by the delta (never the seeded cold-scan
        // row) and exactly the spliced export's line 1. The refresh proof is
        // the row DIFFERENCE plus the per-row content-key tie: `scanned_at`
        // has one-second resolution, so under a serial e2e run the cold scan
        // and the forced rebuild can land in the same wall-clock second and
        // a `scanned_at`-only difference is NOT a reliable discriminator. The
        // full-rebuild comparison is on the identity fields (sha/clean), with
        // the line-1 ties asserted per scan.
        let spliced_scan = db_scan_row(&spliced_db);
        assert_ne!(
            spliced_scan, cold_scan,
            "the seeded Scan row must be deleted+reinserted, not preserved"
        );
        let spliced_jsonl_text = std::fs::read_to_string(&spliced_jsonl).unwrap();
        let spliced_first = spliced_jsonl_text.lines().next().unwrap();
        let sv: serde_json::Value = serde_json::from_str(spliced_first).unwrap();
        assert_eq!(
            sv["type"], "scan_meta",
            "line 1 leads with scan_meta: {spliced_first}"
        );
        assert_eq!(sv["git_sha"].as_str().unwrap_or_default(), spliced_scan.0);
        assert_eq!(
            sv["git_clean"].as_bool().unwrap_or(false),
            spliced_scan.1 == "true"
        );
        assert_eq!(
            sv["content_key"].as_str().unwrap_or_default(),
            spliced_scan.2
        );
        assert_eq!(
            sv["scanned_at"].as_str().unwrap_or_default(),
            spliced_scan.3
        );

        // The full rebuild's Scan row/line 1, and the identity fields it must
        // share with the spliced one (same tree, same HEAD, same dirty state).
        let full_scan = db_scan_row(&db_path);
        assert_eq!(
            (spliced_scan.0.clone(), spliced_scan.1.clone()),
            (full_scan.0.clone(), full_scan.1.clone()),
            "the spliced Scan row's sha/clean must match a full rebuild's"
        );
        let full_jsonl = std::fs::read_to_string(&jsonl_path).unwrap();
        let full_first = full_jsonl.lines().next().unwrap();
        let fv: serde_json::Value = serde_json::from_str(full_first).unwrap();
        assert_eq!(fv["type"], "scan_meta", "line 1 leads with scan_meta");
        assert_eq!(fv["scanned_at"].as_str().unwrap_or_default(), full_scan.3);
        assert_eq!(fv["content_key"].as_str().unwrap_or_default(), full_scan.2);

        // A sample spine query (a bare scratch repo has no authored nodes, so
        // both sides are empty — the equality is still a real assertion).
        let spine = "MATCH (r:Requirement)-[:Drives]->(:Entity)-[:RealisedBy]->\
                     (:Container)-[:SpecImplementedBy]->(c) RETURN r.fqn, c.fqn";
        assert_eq!(
            db_rows(&spliced_db, spine),
            db_rows(&db_path, spine),
            "the spine query must agree with a full rebuild"
        );

        // The EXPORT: record-set equality with the full rebuild's for every
        // record kind EXCEPT the per-scan `scan_meta` line (each scan's own
        // line 1 is tied to its own Scan row above), plus a JSONL → Graph →
        // JSONL round-trip through the re-ingest reader.
        let canon = |t: &str| -> std::collections::BTreeSet<String> {
            t.lines()
                .filter(|l| !l.starts_with("{\"type\":\"scan_meta\""))
                .map(str::to_string)
                .collect()
        };
        assert_eq!(
            canon(&spliced_jsonl_text),
            canon(&full_jsonl),
            "every non-scan_meta export record must equal a full rebuild's"
        );
        let back = read_graph_jsonl(&spliced_jsonl).unwrap();
        let again = repo.root.join("again.jsonl");
        apg::load::write_graph_jsonl(&back, &again).unwrap();
        let again_text = std::fs::read_to_string(&again).unwrap();
        assert_eq!(
            canon(&spliced_jsonl_text),
            canon(&again_text),
            "graph.jsonl must round-trip through read_graph_jsonl"
        );
        // …including its `scan_meta` line 1 (the Scan node round-trips too).
        assert_eq!(
            again_text.lines().next().unwrap(),
            spliced_first,
            "the round-tripped export must reproduce the scan_meta line"
        );

        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&snapshot_dir);
        let _ = std::fs::remove_dir_all(&repo.root);
    }
}
