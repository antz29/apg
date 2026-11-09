//! End-to-end tests for the OO sample fixtures under `tests/oo/` and the COBOL-85 keyword
//! regression: the built `cobolfrontend` runs over scratch copies of the fixtures under each
//! dialect and the emitted facts are checked. Each test writes its own scratch tree under the
//! platform temp dir and removes it on drop; run via `cargo test e2e:: -- --ignored`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;

static NEXT: AtomicU64 = AtomicU64::new(0);

/// The OO sample fixtures (every file the OO cases need, copybook included).
const OO_SAMPLES: [&str; 10] = [
    "class_account.cob",
    "interface_printable.cob",
    "class_savings.cob",
    "object_reference.cob",
    "property_microfocus.cob",
    "invoke_receivers.cob",
    "raise_exception.cob",
    "repository_external.cob",
    "copy_in_method.cob",
    "acctrec.cpy",
];

/// The dialects the OO samples must yield facts under.
const OO_DIALECTS: [&str; 2] = ["-std=cobol2002", "-std=cobol2014"];

/// The dialects the keyword regression compares.
const KW_DIALECTS: [&str; 4] = [
    "-std=cobol85",
    "-std=cobol2002",
    "-std=cobol2014",
    "-std=ibm",
];

/// A scratch tree under the platform temp dir (canonical, so emitted paths match), removed on drop.
struct Scratch {
    root: PathBuf,
}

impl Scratch {
    /// A fresh tree holding a copy of each named fixture from `tests/oo/`, at the same name.
    fn with_fixtures(tag: &str, names: &[&str]) -> Self {
        let root = std::env::temp_dir().join(format!(
            "cobollib-oo-e2e-{}-{tag}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create scratch root");
        let root = std::fs::canonicalize(&root).expect("canonicalize scratch root");
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/oo");
        for name in names {
            std::fs::copy(fixtures.join(name), root.join(name)).expect("copy fixture");
        }
        Scratch { root }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// The OO sample tree: every OO fixture, so cross-file names resolve.
fn oo_samples(tag: &str) -> Scratch {
    Scratch::with_fixtures(tag, &OO_SAMPLES)
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

/// How many `kind` edges run from `from` to `to`.
fn edge_count(recs: &[Value], kind: &str, from: &str, to: &str) -> usize {
    recs.iter()
        .filter(|r| r["type"] == kind && r["from"] == from && r["to"] == to)
        .count()
}

/// Whether a `kind` edge runs from `from` to `to`.
fn has(recs: &[Value], kind: &str, from: &str, to: &str) -> bool {
    edge_count(recs, kind, from, to) > 0
}

/// Whether an INVOKE `calls` edge (`properties.kind` = `invoke`) runs from `from` to `to`.
fn invokes(recs: &[Value], from: &str, to: &str) -> bool {
    recs.iter().any(|r| {
        r["type"] == "calls"
            && r["from"] == from
            && r["to"] == to
            && r["properties"]["kind"] == "invoke"
    })
}

/// The category of the unresolved target `fqn`.
fn category(recs: &[Value], fqn: &str) -> Option<String> {
    recs.iter()
        .find(|r| r["type"] == "unresolved" && r["fqn"] == fqn)
        .and_then(|r| r["category"].as_str().map(str::to_string))
}

/// A record with its opaque id removed and its paths reduced to file names, so two runs over
/// different scratch trees compare equal.
fn plain(r: &Value) -> Value {
    let mut v = r.clone();
    if let Some(o) = v.as_object_mut() {
        o.remove("id");
        for field in ["path", "file"] {
            let base = o
                .get(field)
                .and_then(Value::as_str)
                .and_then(|p| Path::new(p).file_name())
                .map(|n| n.to_string_lossy().into_owned());
            if let Some(base) = base {
                o.insert(field.to_string(), Value::String(base));
            }
        }
    }
    v
}

/// The facts of `out` as sorted lines, each edge endpoint id replaced by its node's content.
fn normalise(out: &Output) -> Vec<String> {
    let recs = records(out);
    let keys: BTreeMap<String, Value> = recs
        .iter()
        .filter_map(|r| Some((r["id"].as_str()?.to_string(), plain(r))))
        .collect();
    let mut lines: Vec<String> = recs
        .iter()
        .map(|r| {
            let mut v = plain(r);
            for field in ["from", "to"] {
                if let Some(node) = r[field].as_str().and_then(|s| keys.get(s)) {
                    v[field] = node.clone();
                }
            }
            v.to_string()
        })
        .collect();
    lines.sort();
    lines
}

/// The facts of one fixture scanned alone under `std`, normalised.
fn kw_facts(name: &str, std: &str) -> Vec<String> {
    let scratch = Scratch::with_fixtures(name, &[name]);
    let out = run(&scratch.root, &[std]);
    assert!(out.status.success(), "{std}: {:?}", stderr_lines(&out));
    normalise(&out)
}

mod e2e {
    use super::*;

    #[test]
    #[ignore = "e2e tier: spawns cobolfrontend over a scratch tree; run via cargo test e2e:: -- --ignored"]
    fn oo_samples_yield_class_interface_method_and_property_structs() {
        for std in OO_DIALECTS {
            let scratch = oo_samples("structs");
            let out = run(&scratch.root, &[std]);
            assert!(out.status.success(), "{std}: {:?}", stderr_lines(&out));
            let recs = records(&out);
            for (name, file) in [
                ("Account", "class_account.cob"),
                ("Printable", "interface_printable.cob"),
                ("Savings", "class_savings.cob"),
                ("Person", "property_microfocus.cob"),
                ("Ledger", "copy_in_method.cob"),
                ("Validator", "raise_exception.cob"),
            ] {
                assert!(
                    path_ends(node(&recs, "struct", name, ""), file),
                    "{std} {name}"
                );
            }
            for (name, file) in [
                ("refdemo", "object_reference.cob"),
                ("invokes", "invoke_receivers.cob"),
                ("jlist", "repository_external.cob"),
            ] {
                assert!(
                    path_ends(node(&recs, "struct", name, ""), file),
                    "{std} {name}"
                );
            }
            // Method Functions under their class; params are the factory/instance key, absent on
            // an interface prototype.
            let methods = [
                ("Account", "create", Some("factory")),
                ("Account", "deposit", Some("instance")),
                ("Account", "balance-of", Some("instance")),
                ("Printable", "render", None),
                ("Printable", "width", None),
                ("Savings", "render", Some("instance")),
                ("Savings", "deposit", Some("instance")),
                ("Person", "GET-Owner", Some("instance")),
                ("Person", "SET-Owner", Some("instance")),
                ("Person", "GET-total-amount", Some("instance")),
                ("Person", "GET-Age", Some("instance")),
                ("Ledger", "first-entry", Some("instance")),
            ];
            for (class, name, params) in methods {
                let f = node(&recs, "function", name, class);
                match params {
                    Some(p) => assert_eq!(f["params"], serde_json::json!([p]), "{std} {name}"),
                    None => assert!(f.get("params").is_none(), "{std} {name}: {f}"),
                }
                let class_id = id(&recs, "struct", class, "");
                assert!(
                    has(&recs, "contains", &class_id, f["id"].as_str().expect("id")),
                    "{std} {class}.{name} not contained by its class"
                );
            }
            // A WITH NO SET property has no SET- accessor.
            assert!(!recs.iter().any(|r| r["type"] == "function"
                && matches!(r["name"].as_str(), Some("SET-total-amount" | "SET-Age"))));
        }
    }

    #[test]
    #[ignore = "e2e tier: spawns cobolfrontend over a scratch tree; run via cargo test e2e:: -- --ignored"]
    fn oo_samples_inherit_implement_and_reference_types() {
        for std in OO_DIALECTS {
            let scratch = oo_samples("uses");
            let out = run(&scratch.root, &[std]);
            assert!(out.status.success(), "{std}: {:?}", stderr_lines(&out));
            let recs = records(&out);
            let savings = id(&recs, "struct", "Savings", "");
            let account = id(&recs, "struct", "Account", "");
            let printable = id(&recs, "struct", "Printable", "");
            // INHERITS and IMPLEMENTS of scanned types each link once.
            assert_eq!(edge_count(&recs, "uses", &savings, &account), 1, "{std}");
            assert_eq!(edge_count(&recs, "uses", &savings, &printable), 1, "{std}");
            // INTERFACE-ID INHERITS FROM a type outside the scan.
            assert!(has(&recs, "unresolved_use", &printable, "Named"), "{std}");
            assert_eq!(category(&recs, "Named").as_deref(), Some("external"));
            // OBJECT REFERENCE: a 01 record, a 05 under a group (owned by the record), and a 77
            // item (owned by the program); an untyped one refers to no class.
            let acct = id(&recs, "struct", "acct", "refdemo");
            let holder = id(&recs, "struct", "holder", "refdemo");
            let any_obj = id(&recs, "struct", "any-obj", "refdemo");
            let program = id(&recs, "struct", "refdemo", "");
            assert!(has(&recs, "uses", &acct, &account), "{std}");
            assert!(has(&recs, "uses", &holder, &account), "{std}");
            assert!(has(&recs, "uses", &program, &account), "{std}");
            assert!(!recs
                .iter()
                .any(|r| r["type"] == "uses" && r["from"] == any_obj.as_str()));
            // MOVE acct TO any-obj in the paragraph: its operands are uses from the paragraph.
            let main = id(&recs, "function", "MAIN-PARA", "refdemo");
            assert!(has(&recs, "uses", &main, &acct), "{std}");
            assert!(has(&recs, "uses", &main, &any_obj), "{std}");
            // Classes outside the scan are external unresolved uses, one per owner.
            let validator = id(&recs, "struct", "Validator", "");
            assert_eq!(
                edge_count(&recs, "unresolved_use", &validator, "Failure"),
                1
            );
            assert_eq!(category(&recs, "Failure").as_deref(), Some("external"));
            let items = id(&recs, "struct", "items", "jlist");
            let jlist = id(&recs, "struct", "jlist", "");
            assert!(has(&recs, "unresolved_use", &items, "JavaList"), "{std}");
            assert!(has(&recs, "unresolved_use", &jlist, "JavaList"), "{std}");
            assert_eq!(category(&recs, "JavaList").as_deref(), Some("external"));
            let ext = id(&recs, "struct", "ext", "invokes");
            assert!(has(&recs, "unresolved_use", &ext, "Widget"), "{std}");
            assert_eq!(category(&recs, "Widget").as_deref(), Some("external"));
        }
    }

    #[test]
    #[ignore = "e2e tier: spawns cobolfrontend over a scratch tree; run via cargo test e2e:: -- --ignored"]
    fn copy_in_a_class_method_is_a_copy_use_from_the_class() {
        for std in OO_DIALECTS {
            let scratch = oo_samples("copy");
            let out = run(&scratch.root, &[std]);
            assert!(out.status.success(), "{std}: {:?}", stderr_lines(&out));
            let recs = records(&out);
            let ledger = id(&recs, "struct", "Ledger", "");
            let copybook = recs
                .iter()
                .find(|r| r["type"] == "file" && path_ends(r, "acctrec.cpy"))
                .unwrap_or_else(|| panic!("no copybook File: {recs:?}"));
            let to = copybook["path"].as_str().expect("copybook path");
            let copy_edges: Vec<&Value> = recs
                .iter()
                .filter(|r| r["type"] == "uses" && r["properties"]["kind"] == "copy")
                .collect();
            assert_eq!(copy_edges.len(), 1, "{std}: {copy_edges:?}");
            assert_eq!(copy_edges[0]["from"], ledger.as_str(), "{std}");
            assert_eq!(copy_edges[0]["to"], to, "{std}");
        }
    }

    #[test]
    #[ignore = "e2e tier: spawns cobolfrontend over a scratch tree; run via cargo test e2e:: -- --ignored"]
    fn invoke_calls_resolve_by_receiver_and_unresolved_receivers_are_unresolved_calls() {
        for std in OO_DIALECTS {
            let scratch = oo_samples("invoke");
            let out = run(&scratch.root, &[std]);
            assert!(out.status.success(), "{std}: {:?}", stderr_lines(&out));
            let recs = records(&out);
            let main = id(&recs, "function", "main-para", "invokes");
            let create = id(&recs, "function", "create", "Account");
            let deposit = id(&recs, "function", "deposit", "Account");
            let balance = id(&recs, "function", "balance-of", "Account");
            // A class-name receiver goes to its factory; a typed receiver to its instance method.
            assert!(invokes(&recs, &main, &create), "{std}");
            assert!(invokes(&recs, &main, &deposit), "{std}");
            assert!(invokes(&recs, &main, &balance), "{std}");
            // SELF and SUPER resolve through the class and its parent.
            let render = id(&recs, "function", "render", "Savings");
            let savings_deposit = id(&recs, "function", "deposit", "Savings");
            assert!(
                invokes(&recs, &render, &savings_deposit),
                "{std}: {:?}",
                stderr_lines(&out)
            );
            assert!(invokes(&recs, &savings_deposit, &deposit), "{std}");
            // An untyped receiver, and an external class, are unresolved calls from the caller.
            assert!(has(&recs, "unresolved_call", &main, "poke"), "{std}");
            // SELF with no `new` in its section, and an external class's `new`.
            assert!(has(&recs, "unresolved_call", &create, "new"), "{std}");
            let jlist_main = id(&recs, "function", "main-para", "jlist");
            assert!(has(&recs, "unresolved_call", &jlist_main, "new"), "{std}");
        }
    }

    #[test]
    #[ignore = "e2e tier: spawns cobolfrontend over a scratch tree; run via cargo test e2e:: -- --ignored"]
    fn raise_adds_no_calls_or_uses() {
        for std in OO_DIALECTS {
            let scratch = oo_samples("raise");
            let out = run(&scratch.root, &[std]);
            assert!(out.status.success(), "{std}: {:?}", stderr_lines(&out));
            let recs = records(&out);
            // The RAISE statements parse (no file errors) and `check` is still emitted.
            assert_eq!(
                stderr_lines(&out).last().map(String::as_str),
                Some("cobol: 0 files had errors"),
                "{std}"
            );
            let check = id(&recs, "function", "check", "Validator");
            assert!(
                !recs.iter().any(|r| r["from"] == check.as_str()
                    && matches!(
                        r["type"].as_str(),
                        Some("calls" | "unresolved_call" | "uses" | "unresolved_use")
                    )),
                "{std}: {recs:?}"
            );
        }
    }

    #[test]
    #[ignore = "e2e tier: spawns cobolfrontend over a scratch tree; run via cargo test e2e:: -- --ignored"]
    fn cobol85_yields_no_class_or_interface_structs() {
        let scratch = oo_samples("cobol85");
        let out = run(&scratch.root, &["-std=cobol85"]);
        assert!(out.status.success(), "{:?}", stderr_lines(&out));
        let recs = records(&out);
        for name in [
            "Account",
            "Printable",
            "Savings",
            "Person",
            "Ledger",
            "Validator",
        ] {
            assert!(
                !recs
                    .iter()
                    .any(|r| r["type"] == "struct" && r["name"] == name),
                "{name}: {recs:?}"
            );
        }
        assert!(!recs
            .iter()
            .any(|r| r["type"] == "calls" && r["properties"]["kind"] == "invoke"));
    }

    #[test]
    #[ignore = "e2e tier: spawns cobolfrontend over a scratch tree; run via cargo test e2e:: -- --ignored"]
    fn valid_oo_samples_have_zero_files_with_errors() {
        for std in OO_DIALECTS {
            let scratch = oo_samples("errors");
            let out = run(&scratch.root, &[std]);
            assert!(out.status.success(), "{std}: {:?}", stderr_lines(&out));
            assert_eq!(
                stderr_lines(&out).last().map(String::as_str),
                Some("cobol: 0 files had errors"),
                "{std}: {:?}",
                stderr_lines(&out)
            );
        }
    }

    #[test]
    #[ignore = "e2e tier: spawns cobolfrontend over a scratch tree; run via cargo test e2e:: -- --ignored"]
    fn kw_data_names_85_facts_are_identical_across_dialects() {
        let base = kw_facts("kw_data_names_85.cob", KW_DIALECTS[0]);
        assert!(base.iter().any(|l| l.contains("\"name\":\"kwnames\"")));
        for std in &KW_DIALECTS[1..] {
            assert_eq!(kw_facts("kw_data_names_85.cob", std), base, "{std}");
        }
    }

    #[test]
    #[ignore = "e2e tier: spawns cobolfrontend over a scratch tree; run via cargo test e2e:: -- --ignored"]
    fn kw_data_names_oo_85_facts_are_identical_across_dialects() {
        let base = kw_facts("kw_data_names_oo_85.cob", KW_DIALECTS[0]);
        assert!(base.iter().any(|l| l.contains("\"name\":\"kwoo\"")));
        for std in &KW_DIALECTS[1..] {
            assert_eq!(kw_facts("kw_data_names_oo_85.cob", std), base, "{std}");
        }
    }

    #[test]
    #[ignore = "e2e tier: spawns cobolfrontend over a scratch tree; run via cargo test e2e:: -- --ignored"]
    fn kw_class_85_is_a_diagnostic_under_cobol85() {
        let scratch = Scratch::with_fixtures("kw-class", &["kw_class_85.cob"]);
        let out = run(&scratch.root, &["-std=cobol85"]);
        assert!(out.status.success(), "{:?}", stderr_lines(&out));
        let stderr = stderr_lines(&out);
        assert!(
            stderr.iter().any(|l| l.contains("kw_class_85.cob:5:")),
            "{stderr:?}"
        );
        assert_eq!(
            stderr.last().map(String::as_str),
            Some("cobol: 1 file had errors"),
            "{stderr:?}"
        );
        let recs = records(&out);
        assert_eq!(node(&recs, "struct", "kwclass", "")["parent"], "");
    }
}
