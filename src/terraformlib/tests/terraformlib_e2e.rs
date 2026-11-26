//! The crate's e2e tier: the built `terraformfrontend` binary run over fixture trees
//! written to a scratch temp dir, its JSONL stdout and stderr read back. No user path,
//! `$HOME` or real project is read.

use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;

/// A unique scratch directory under the system temp dir, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("terraformlib-e2e-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create scratch dir");
        Scratch(dir)
    }

    /// Writes `bytes` to `name` (parents created) inside the scratch dir and returns its path.
    fn write(&self, name: &str, bytes: impl AsRef<[u8]>) -> PathBuf {
        let path = self.0.join(name);
        fs::create_dir_all(path.parent().expect("parent")).expect("create fixture dir");
        fs::write(&path, bytes).expect("write fixture");
        path
    }

    /// The scratch dir, symlink-resolved: the identity the binary's records carry.
    fn root(&self) -> String {
        fs::canonicalize(&self.0)
            .expect("canonical scratch")
            .to_string_lossy()
            .into_owned()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// One `terraformfrontend` run: stdout, stderr lines and whether it exited 0.
struct Run {
    stdout: String,
    stderr: Vec<String>,
    ok: bool,
}

/// Runs the built `terraformfrontend` binary with `args`.
fn tf(args: &[&str]) -> Run {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_terraformfrontend"))
        .args(args)
        .output()
        .expect("spawn terraformfrontend");
    Run {
        stdout: String::from_utf8(out.stdout).expect("UTF-8 stdout"),
        stderr: String::from_utf8_lossy(&out.stderr)
            .lines()
            .map(str::to_string)
            .collect(),
        ok: out.status.success(),
    }
}

/// Every stdout line parsed as one JSON object.
fn records(stdout: &str) -> Vec<Value> {
    stdout
        .lines()
        .map(|l| {
            let v: Value = serde_json::from_str(l).expect("one JSON value per line");
            assert!(v.is_object(), "{l}");
            v
        })
        .collect()
}

/// The records of one `type`.
fn of<'a>(recs: &'a [Value], kind: &str) -> Vec<&'a Value> {
    recs.iter().filter(|r| r["type"] == kind).collect()
}

/// The string at `key` of `v`.
fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().expect(key)
}

/// The one Struct record named `name` of kind `kind`.
fn named<'a>(recs: &'a [Value], kind: &str, name: &str) -> &'a Value {
    recs.iter()
        .find(|r| r["type"] == kind && r["name"] == name)
        .unwrap_or_else(|| panic!("no {kind} named {name}"))
}

fn pair(a: &str, b: &str) -> (String, String) {
    (a.to_string(), b.to_string())
}

/// Each Module fqn, sorted.
fn modules(recs: &[Value]) -> Vec<String> {
    let mut v: Vec<String> = of(recs, "module")
        .iter()
        .map(|r| text(r, "fqn").to_string())
        .collect();
    v.sort();
    v
}

/// Each File as (absolute path, parent module, start line, end line), sorted.
fn files(recs: &[Value]) -> Vec<(String, String, u64, u64)> {
    let mut v: Vec<_> = of(recs, "file")
        .iter()
        .map(|r| {
            (
                text(r, "path").to_string(),
                text(r, "parent").to_string(),
                r["start_line"].as_u64().expect("start_line"),
                r["end_line"].as_u64().expect("end_line"),
            )
        })
        .collect();
    v.sort();
    v
}

/// Struct id to name.
fn names(recs: &[Value]) -> HashMap<String, String> {
    of(recs, "struct")
        .iter()
        .map(|r| (text(r, "id").to_string(), text(r, "name").to_string()))
        .collect()
}

/// Each Struct as (name, parent), sorted.
fn structs(recs: &[Value]) -> Vec<(String, String)> {
    let mut v: Vec<_> = of(recs, "struct")
        .iter()
        .map(|r| pair(text(r, "name"), text(r, "parent")))
        .collect();
    v.sort();
    v
}

/// Each struct-to-struct Uses as (from name, to name), sorted. An unnamed target keeps its id.
fn uses(recs: &[Value]) -> Vec<(String, String)> {
    let by_id = names(recs);
    let mut v: Vec<_> = of(recs, "uses")
        .iter()
        .filter(|r| r.get("properties").is_none())
        .map(|r| {
            let to = text(r, "to");
            (
                by_id[text(r, "from")].clone(),
                by_id.get(to).cloned().unwrap_or(to.to_string()),
            )
        })
        .collect();
    v.sort();
    v
}

/// Each file-dependency Uses as (from name, target path, kind), sorted.
fn file_uses(recs: &[Value]) -> Vec<(String, String, String)> {
    let by_id = names(recs);
    let mut v: Vec<_> = of(recs, "uses")
        .iter()
        .filter(|r| r.get("properties").is_some())
        .map(|r| {
            (
                by_id[text(r, "from")].clone(),
                text(r, "to").to_string(),
                text(&r["properties"], "kind").to_string(),
            )
        })
        .collect();
    v.sort();
    v
}

/// Each Contains as (parent name, child name), sorted.
fn contains(recs: &[Value]) -> Vec<(String, String)> {
    let by_id = names(recs);
    let mut v: Vec<_> = of(recs, "contains")
        .iter()
        .map(|r| pair(&by_id[text(r, "from")], &by_id[text(r, "to")]))
        .collect();
    v.sort();
    v
}

/// Every record as its canonical JSON text, sorted: the order-free set of a run.
fn sorted(recs: &[Value]) -> Vec<String> {
    let mut v: Vec<String> = recs.iter().map(Value::to_string).collect();
    v.sort();
    v
}

/// The records `scan::retain` keeps for `targets` (absolute paths), derived from the full
/// scan's records: the kept Files and Structs, their Module, the Contains and Uses between
/// kept Structs (a Uses into a dropped Struct retargeted to that Struct's key), and the
/// unresolved uses they name.
fn retained(full: &[Value], targets: &[String]) -> Vec<String> {
    let hit = |r: &Value| targets.iter().any(|t| r["path"] == t.as_str());
    let kept: HashSet<String> = of(full, "struct")
        .into_iter()
        .filter(|r| hit(r))
        .map(|r| text(r, "id").to_string())
        .collect();
    let dirs: HashSet<String> = of(full, "file")
        .into_iter()
        .filter(|r| hit(r))
        .map(|r| text(r, "parent").to_string())
        .collect();
    let keys: HashMap<String, String> = of(full, "struct")
        .into_iter()
        .map(|r| {
            (
                text(r, "id").to_string(),
                format!("{}.{}", text(r, "parent"), text(r, "name")),
            )
        })
        .collect();
    let named: HashSet<String> = of(full, "unresolved_use")
        .into_iter()
        .filter(|r| kept.contains(text(r, "from")))
        .map(|r| text(r, "to").to_string())
        .collect();
    let mut out = Vec::new();
    for r in full {
        let mut r = r.clone();
        let kind = text(&r, "type").to_string();
        let keep = match kind.as_str() {
            "module" => dirs.contains(text(&r, "fqn")),
            "file" | "struct" => hit(&r),
            "contains" => kept.contains(text(&r, "from")) && kept.contains(text(&r, "to")),
            "uses" => {
                let from_kept = kept.contains(text(&r, "from"));
                let to = text(&r, "to").to_string();
                if let Some(k) = keys.get(&to).filter(|_| !kept.contains(&to)) {
                    r["to"] = k.clone().into();
                }
                from_kept
            }
            "unresolved_use" => kept.contains(text(&r, "from")),
            "unresolved" => named.contains(text(&r, "fqn")),
            _ => false,
        };
        if keep {
            out.push(r.to_string());
        }
    }
    out.sort();
    out
}

/// The two-module fixture: a root `a.tf` and `b.tf` (an output reading `var.v`, which lives
/// in `a.tf`, and a module call to `./net`), and `net/main.tf`.
fn split_tree(dir: &Scratch) {
    dir.write("a.tf", "variable \"v\" {}\n");
    dir.write(
        "b.tf",
        "output \"o\" {\n  value = var.v\n}\nmodule \"net\" {\n  source = \"./net\"\n}\n",
    );
    dir.write("net/main.tf", "variable \"c\" {}\n");
}

mod e2e {
    use super::*;

    #[test]
    #[ignore = "e2e tier: real filesystem and process; run via cargo test-e2e"]
    fn tree_emits_modules_files_structs_and_edges() {
        let dir = Scratch::new("tree");
        let main = "variable \"region\" {}\nresource \"aws_vpc\" \"main\" {\n  cidr_block = var.region\n}\nmodule \"net\" {\n  source = \"./net\"\n}\n";
        dir.write("main.tf", main);
        dir.write("net/main.tf", "variable \"cidr\" {}\n");
        dir.write(".terraform/modules/skip.tf", "variable \"skip\" {}\n");
        let root = dir.root();
        let run = tf(&[&root]);
        assert!(run.ok, "{:?}", run.stderr);
        assert_eq!(run.stderr, ["terraform: 0 files had syntax errors"]);
        let recs = records(&run.stdout);
        assert_eq!(modules(&recs), ["", "net"]);
        assert_eq!(
            files(&recs),
            [
                (
                    format!("{root}/main.tf"),
                    String::new(),
                    1,
                    main.lines().count() as u64
                ),
                (format!("{root}/net/main.tf"), "net".to_string(), 1, 1),
            ]
        );
        assert_eq!(
            structs(&recs),
            [
                pair("aws_vpc.main", ""),
                pair("module.net", ""),
                pair("var.cidr", "net"),
                pair("var.region", ""),
            ]
        );
        assert_eq!(uses(&recs), [pair("aws_vpc.main", "var.region")]);
        assert_eq!(
            file_uses(&recs),
            [(
                "module.net".to_string(),
                format!("{root}/net/main.tf"),
                "module-source".to_string()
            )]
        );
        assert!(!run.stdout.contains(".terraform/"), "{}", run.stdout);
    }

    #[test]
    #[ignore = "e2e tier: real filesystem and process; run via cargo test-e2e"]
    fn run_structs_sit_under_their_test_struct() {
        let dir = Scratch::new("runs");
        dir.write("tests/x.tftest.hcl", "run \"smoke\" {}\n");
        let root = dir.root();
        let run = tf(&[&root]);
        assert!(run.ok, "{:?}", run.stderr);
        let recs = records(&run.stdout);
        assert_eq!(modules(&recs), [""]);
        let test = named(&recs, "struct", "test.tests/x.tftest.hcl");
        let smoke = named(&recs, "struct", "run.smoke");
        assert_eq!(
            contains(&recs),
            [pair("test.tests/x.tftest.hcl", "run.smoke")]
        );
        assert_eq!(
            text(smoke, "parent"),
            format!("{}.{}", text(test, "parent"), text(test, "name"))
        );
    }

    #[test]
    #[ignore = "e2e tier: real filesystem and process; run via cargo test-e2e"]
    fn syntax_error_keeps_file_and_reports_and_scan_continues() {
        let dir = Scratch::new("syntax");
        let broken = "resource \"a\" \"b\" {\n  x = = 1\n}\n";
        dir.write("broken.tf", broken);
        dir.write("clean.tf", "variable \"v\" {}\n");
        let root = dir.root();
        let run = tf(&[&root]);
        assert!(run.ok, "{:?}", run.stderr);
        let prefix = format!("terraform: {root}/broken.tf:");
        let diags: Vec<&String> = run
            .stderr
            .iter()
            .filter(|l| l.starts_with(prefix.as_str()))
            .collect();
        assert_eq!(diags.len(), 1, "{:?}", run.stderr);
        let rest = &diags[0][prefix.len()..];
        assert!(rest.starts_with("2:"), "{rest}");
        assert!(rest
            .split(':')
            .nth(1)
            .is_some_and(|c| c.parse::<usize>().is_ok()));
        assert_eq!(
            run.stderr.len(),
            2,
            "one diagnostic plus the closing count: {:?}",
            run.stderr
        );
        assert_eq!(
            run.stderr.last().map(String::as_str),
            Some("terraform: 1 file had syntax errors")
        );
        let recs = records(&run.stdout);
        assert_eq!(
            files(&recs),
            [
                (
                    format!("{root}/broken.tf"),
                    String::new(),
                    1,
                    broken.lines().count() as u64
                ),
                (format!("{root}/clean.tf"), String::new(), 1, 1),
            ]
        );
        assert!(of(&recs, "struct")
            .iter()
            .all(|r| !text(r, "path").ends_with("broken.tf")));
        assert_eq!(structs(&recs), [pair("var.v", "")]);
    }

    #[test]
    #[ignore = "e2e tier: real process; run via cargo test-e2e"]
    fn missing_argument_fails_with_usage_reason() {
        let run = tf(&[]);
        assert!(!run.ok);
        let last = run.stderr.last().expect("a stderr line");
        assert!(last.starts_with("terraformfrontend: usage: "), "{last}");
    }

    #[test]
    #[ignore = "e2e tier: real filesystem and process; run via cargo test-e2e"]
    fn tofu_shadows_tf_keeping_file_record() {
        let dir = Scratch::new("shadow");
        dir.write("main.tf", "variable \"a\" {}\n");
        dir.write("main.tofu", "variable \"b\" {}\n");
        let root = dir.root();
        let run = tf(&[&root]);
        assert!(run.ok, "{:?}", run.stderr);
        let recs = records(&run.stdout);
        assert_eq!(
            files(&recs),
            [
                (format!("{root}/main.tf"), String::new(), 1, 1),
                (format!("{root}/main.tofu"), String::new(), 1, 1),
            ]
        );
        assert_eq!(structs(&recs), [pair("var.b", "")]);
    }

    #[test]
    #[ignore = "e2e tier: real filesystem and process; run via cargo test-e2e"]
    fn targets_of_every_file_equal_the_full_scan() {
        let dir = Scratch::new("targets-all");
        split_tree(&dir);
        let root = dir.root();
        let full = tf(&[&root]);
        assert!(full.ok, "{:?}", full.stderr);
        let side = Scratch::new("targets-all-side");
        let list = side.write(
            "targets.txt",
            format!("{root}/a.tf\n{root}/b.tf\n{root}/net/main.tf\n"),
        );
        let run = tf(&[&root, "--targets", list.to_str().expect("UTF-8 path")]);
        assert!(run.ok, "{:?}", run.stderr);
        assert_eq!(
            sorted(&records(&run.stdout)),
            sorted(&records(&full.stdout))
        );
    }

    #[test]
    #[ignore = "e2e tier: real filesystem and process; run via cargo test-e2e"]
    fn single_target_keeps_its_facts_and_retargets_dropped_uses() {
        let dir = Scratch::new("targets-one");
        split_tree(&dir);
        let root = dir.root();
        let full = tf(&[&root]);
        assert!(full.ok, "{:?}", full.stderr);
        let target = format!("{root}/b.tf");
        let side = Scratch::new("targets-one-side");
        let list = side.write("targets.txt", format!("{target}\n"));
        let run = tf(&[&root, "--targets", list.to_str().expect("UTF-8 path")]);
        assert!(run.ok, "{:?}", run.stderr);
        // output.o reads var.v, which lives in a.tf: dropped, so the use names its key
        assert!(run.stdout.contains("\".var.v\""), "{}", run.stdout);
        let mut got: Vec<String> = records(&run.stdout).iter().map(Value::to_string).collect();
        got.sort();
        assert_eq!(got, retained(&records(&full.stdout), &[target]));
    }

    #[test]
    #[ignore = "e2e tier: real filesystem and process; run via cargo test-e2e"]
    fn exclude_argument_drops_matching_paths() {
        let dir = Scratch::new("exclude");
        dir.write("main.tf", "variable \"a\" {}\n");
        dir.write("vendored/x.tf", "variable \"x\" {}\n");
        let root = dir.root();
        let run = tf(&[&root, "vendored"]);
        assert!(run.ok, "{:?}", run.stderr);
        let recs = records(&run.stdout);
        assert_eq!(modules(&recs), [""]);
        assert_eq!(
            files(&recs),
            [(format!("{root}/main.tf"), String::new(), 1, 1)]
        );
        assert_eq!(structs(&recs), [pair("var.a", "")]);
    }

    #[test]
    #[ignore = "e2e tier: real filesystem and process; run via cargo test-e2e"]
    fn cache_flags_change_nothing() {
        let dir = Scratch::new("cache");
        dir.write("main.tf", "variable \"a\" {}\n");
        let root = dir.root();
        let plain = tf(&[&root]);
        let cached = tf(&[&root, "--cache-dir", &root, "--cache-key", "k"]);
        assert!(plain.ok && cached.ok, "{:?}", cached.stderr);
        assert_eq!(cached.stdout, plain.stdout);
        assert_eq!(cached.stderr, plain.stderr);
    }

    #[test]
    #[ignore = "e2e tier: real filesystem and process; run via cargo test-e2e"]
    fn id_prefix_prefixes_every_struct_id() {
        let dir = Scratch::new("prefix");
        dir.write("main.tf", "variable \"a\" {}\nvariable \"b\" {}\n");
        let root = dir.root();
        let run = tf(&[&root, "--id-prefix", "tf"]);
        assert!(run.ok, "{:?}", run.stderr);
        let recs = records(&run.stdout);
        let ids: Vec<&str> = of(&recs, "struct").iter().map(|r| text(r, "id")).collect();
        assert_eq!(ids, ["tf1", "tf2"]);
    }

    #[test]
    #[ignore = "e2e tier: real filesystem and process; run via cargo test-e2e"]
    fn struct_spans_are_raw_bytes_after_bom() {
        let dir = Scratch::new("bom");
        let raw: &[u8] = b"\xEF\xBB\xBFvariable \"b\" {}\n";
        dir.write("bom.tf", raw);
        let root = dir.root();
        let run = tf(&[&root]);
        assert!(run.ok, "{:?}", run.stderr);
        let recs = records(&run.stdout);
        let found = of(&recs, "struct");
        assert_eq!(found.len(), 1);
        let s = found[0];
        assert_eq!(
            (s["start_line"].as_u64(), s["end_line"].as_u64()),
            (Some(1), Some(1))
        );
        let (start, end) = (
            s["start"].as_u64().expect("start") as usize,
            s["end"].as_u64().expect("end") as usize,
        );
        assert_eq!(start, 3, "the BOM is counted in raw offsets");
        let body = std::str::from_utf8(&raw[start..end])
            .expect("UTF-8 span")
            .trim_end();
        assert!(
            body.starts_with("variable") && body.ends_with('}'),
            "{body}"
        );
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "e2e tier: real filesystem and process; run via cargo test-e2e"]
    fn symlinks_are_skipped() {
        let dir = Scratch::new("links");
        dir.write("main.tf", "variable \"a\" {}\n");
        dir.write("net/main.tf", "variable \"b\" {}\n");
        std::os::unix::fs::symlink(dir.0.join("main.tf"), dir.0.join("link.tf")).expect("symlink");
        std::os::unix::fs::symlink(dir.0.join("net"), dir.0.join("linkdir")).expect("symlink");
        let root = dir.root();
        let run = tf(&[&root]);
        assert!(run.ok, "{:?}", run.stderr);
        let recs = records(&run.stdout);
        let paths: Vec<String> = files(&recs).into_iter().map(|f| f.0).collect();
        assert_eq!(
            paths,
            [format!("{root}/main.tf"), format!("{root}/net/main.tf")]
        );
    }
}
