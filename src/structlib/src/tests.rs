use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::builder::Structure;
use crate::config::{glob_match, StructuralScope};
use crate::discovery::{in_scope, is_pruned_dir, stream_for_path, under_excluded_tree};
use crate::formats::dockerfile::emit_dockerfile;
use crate::formats::emit_misc;
use crate::formats::ini::emit_ini;
use crate::formats::json::emit_json;
use crate::formats::makefile::emit_makefile;
use crate::formats::sh::emit_sh;
use crate::formats::toml::emit_toml;
use crate::formats::xml::emit_xml;
use crate::formats::yaml::emit_yaml;
use crate::lines::line_count;
use crate::md::{build_doc, parse_headings, Doc};
use crate::paths::repo_relative_dir;
use crate::record::{write_rec, Rec};

// ── shared helpers (kept at the `mod tests` root, reached by every tier) ──

/// Runs a format emitter over `text` with a fresh id counter.
fn emit(f: fn(&Path, &[u8], &str, &mut u64) -> Structure, path: &str, text: &str) -> Structure {
    let mut next = 1u64;
    f(Path::new(path), text.as_bytes(), "n", &mut next)
}

/// `(name, parent, start_line, end_line)` for every `Struct` record.
fn struct_rows(s: &Structure) -> Vec<(String, String, u32, u32)> {
    s.structs
        .iter()
        .map(|r| match r {
            Rec::Struct {
                parent,
                name,
                start_line,
                end_line,
                ..
            } => (name.clone(), parent.clone(), *start_line, *end_line),
            _ => unreachable!("a Structure holds only Struct records"),
        })
        .collect()
}

/// The `Struct` names, in emission order.
fn names(s: &Structure) -> Vec<String> {
    struct_rows(s).into_iter().map(|(n, ..)| n).collect()
}

/// `id -> rendered FQN (parent.name)` for a Structure's Structs.
fn fqns_by_id(s: &Structure) -> std::collections::HashMap<String, String> {
    s.structs
        .iter()
        .map(|r| match r {
            Rec::Struct {
                id, parent, name, ..
            } => (id.clone(), format!("{parent}.{name}")),
            _ => unreachable!("a Structure holds only Struct records"),
        })
        .collect()
}

/// The `contains` edges of a Structure, resolved to `(from-fqn, to-fqn)`.
fn contains_fqns(s: &Structure) -> Vec<(String, String)> {
    let map = fqns_by_id(s);
    s.contains
        .iter()
        .map(|r| match r {
            Rec::Contains { from, to } => (map[from].clone(), map[to].clone()),
            _ => unreachable!("a Structure holds only Contains records"),
        })
        .collect()
}

/// The absorbed md emitter's record half: `build_doc`'s sections as
/// `Struct`/`contains` facts (mirrors `run`'s md branch).
fn md_structure(doc: &Doc) -> Structure {
    Structure {
        structs: doc
            .sections
            .iter()
            .map(|s| Rec::Struct {
                id: s.id.clone(),
                parent: s.parent.clone(),
                name: s.name.clone(),
                path: doc.path.clone(),
                start: s.start,
                end: s.end,
                start_line: s.start_line,
                end_line: s.end_line,
            })
            .collect(),
        contains: doc
            .sections
            .iter()
            .filter_map(|s| {
                s.parent_index.map(|pi| Rec::Contains {
                    from: doc.sections[pi].id.clone(),
                    to: s.id.clone(),
                })
            })
            .collect(),
    }
}

/// The in-memory record assembly the `int` tests exercise: route each file
/// through its format emitter and assemble the Module/File/Struct/contains
/// records exactly as `run` does, serialized with the real `write_rec`.
/// `selected` mirrors `run`'s per-stream `--stream <id>` retain. No
/// filesystem, no process.
fn assemble(
    base: &Path,
    files: &[(&str, &str, &str)],
    selected: Option<&str>,
) -> Vec<serde_json::Value> {
    let mut next_id = 1u64;
    let mut modules: BTreeSet<String> = BTreeSet::new();
    let mut file_recs: Vec<Rec> = Vec::new();
    let mut struct_recs: Vec<Rec> = Vec::new();
    let mut contains_recs: Vec<Rec> = Vec::new();
    for &(path_str, stream, text) in files {
        if selected.is_some_and(|sel| sel != stream) {
            continue;
        }
        let path = Path::new(path_str);
        let bytes = text.as_bytes();
        let module = repo_relative_dir(base, path);
        let structure = match stream {
            "md" => md_structure(&build_doc(path, bytes, base, "n", &mut next_id)),
            "sh" => emit_sh(path, bytes, "n", &mut next_id),
            "yaml" => emit_yaml(path, bytes, "n", &mut next_id),
            "json" => emit_json(path, bytes, "n", &mut next_id),
            "toml" => emit_toml(path, bytes, "n", &mut next_id),
            "xml" => emit_xml(path, bytes, "n", &mut next_id),
            "dockerfile" => emit_dockerfile(path, bytes, "n", &mut next_id),
            "makefile" => emit_makefile(path, bytes, "n", &mut next_id),
            "ini" => emit_ini(path, bytes, "n", &mut next_id),
            _ => emit_misc(path, bytes, "n", &mut next_id),
        };
        modules.insert(module.clone());
        file_recs.push(Rec::File {
            path: path_str.to_string(),
            parent: module,
            start_line: 1,
            end_line: line_count(text),
        });
        struct_recs.extend(structure.structs);
        contains_recs.extend(structure.contains);
    }
    let mut recs: Vec<Rec> = modules.into_iter().map(|fqn| Rec::Module { fqn }).collect();
    recs.extend(file_recs);
    recs.extend(struct_recs);
    recs.extend(contains_recs);
    let mut buf: Vec<u8> = Vec::new();
    for r in &recs {
        write_rec(&mut buf, r).expect("serialize record");
    }
    String::from_utf8(buf)
        .expect("utf8 jsonl")
        .lines()
        .map(|l| serde_json::from_str(l).expect("parse record"))
        .collect()
}

mod unit {
    use super::*;

    #[test]
    fn sh_functions_are_extracted() {
        let src = "# a comment\n\
                       FOO=bar\n\
                       build_all() {\n\
                       \x20 echo hi\n\
                       }\n\
                       function deploy {\n\
                       \x20 true\n\
                       }\n\
                       helper () { :; }\n\
                       echo done\n";
        let s = emit(emit_sh, "build.sh", src);
        let rows = struct_rows(&s);
        assert_eq!(
            rows.iter().map(|(n, ..)| n.as_str()).collect::<Vec<_>>(),
            vec!["build_all", "deploy", "helper"]
        );
        // Ranges: the definition line through the closing brace.
        assert_eq!((rows[0].2, rows[0].3), (3, 5));
        assert_eq!((rows[1].2, rows[1].3), (6, 8));
        assert_eq!((rows[2].2, rows[2].3), (9, 9));
        assert_eq!(rows[0].1, "build.sh");
        assert!(s.contains.is_empty());
    }

    #[test]
    fn yaml_keys_jobs_and_steps_are_extracted() {
        let src = "name: CI\n\
                       on: push\n\
                       jobs:\n\
                       \x20 build:\n\
                       \x20   runs-on: ubuntu-latest\n\
                       \x20   steps:\n\
                       \x20     - uses: actions/checkout@v4\n\
                       \x20     - run: make\n\
                       \x20 test:\n\
                       \x20   steps:\n\
                       \x20     - run: make test\n\
                       permissions:\n\
                       \x20 contents: read\n";
        let s = emit(emit_yaml, "ci.yml", src);
        let rows = struct_rows(&s);
        assert_eq!(
            rows.iter().map(|(n, ..)| n.as_str()).collect::<Vec<_>>(),
            vec![
                "name",
                "on",
                "jobs",
                "build",
                "step-1",
                "step-2",
                "test",
                "step-1",
                "permissions"
            ]
        );
        let contains = contains_fqns(&s);
        assert!(contains.contains(&("ci.yml.jobs".into(), "ci.yml.jobs.build".into())));
        assert!(contains.contains(&("ci.yml.jobs".into(), "ci.yml.jobs.test".into())));
        assert!(contains.contains(&(
            "ci.yml.jobs.build".into(),
            "ci.yml.jobs.build.step-1".into()
        )));
        assert!(contains.contains(&(
            "ci.yml.jobs.build".into(),
            "ci.yml.jobs.build.step-2".into()
        )));
        assert!(contains.contains(&("ci.yml.jobs.test".into(), "ci.yml.jobs.test.step-1".into())));
    }

    #[test]
    fn json_keys_are_extracted() {
        let src = "{\n\
                       \x20 \"name\": \"x\",\n\
                       \x20 \"nested\": {\n\
                       \x20   \"inner\": 1\n\
                       \x20 },\n\
                       \x20 \"list\": [1, 2]\n\
                       }\n";
        let s = emit(emit_json, "data.json", src);
        let rows = struct_rows(&s);
        assert_eq!(
            rows.iter().map(|(n, ..)| n.as_str()).collect::<Vec<_>>(),
            vec!["name", "nested", "list"]
        );
        assert_eq!(rows.iter().map(|r| r.2).collect::<Vec<_>>(), vec![2, 3, 6]);
        assert!(s.contains.is_empty());
    }

    #[test]
    fn toml_tables_and_keys_are_extracted() {
        let src = "title = \"x\"\n\
                       [package]\n\
                       name = \"y\"\n\
                       version = \"1\"\n\
                       [[bin]]\n\
                       name = \"z\"\n";
        let s = emit(emit_toml, "Cargo.toml", src);
        let rows = struct_rows(&s);
        assert_eq!(
            rows.iter().map(|(n, ..)| n.as_str()).collect::<Vec<_>>(),
            vec!["title", "package", "name", "version", "bin", "name"]
        );
        assert_eq!(rows[0].1, "Cargo.toml");
        assert_eq!(rows[2].1, "Cargo.toml.package");
        assert_eq!(rows[5].1, "Cargo.toml.bin");
        let contains = contains_fqns(&s);
        assert!(contains.contains(&(
            "Cargo.toml.package".into(),
            "Cargo.toml.package.name".into()
        )));
        assert!(contains.contains(&("Cargo.toml.bin".into(), "Cargo.toml.bin.name".into())));
    }

    #[test]
    fn xml_elements_are_extracted() {
        let src = "<project>\n\
                       \x20 <dependency>a</dependency>\n\
                       \x20 <dependency>b</dependency>\n\
                       \x20 <build><plugin/></build>\n\
                       </project>\n";
        let s = emit(emit_xml, "pom.xml", src);
        let rows = struct_rows(&s);
        assert_eq!(
            rows.iter().map(|(n, ..)| n.as_str()).collect::<Vec<_>>(),
            vec!["project", "dependency", "dependency-1", "build", "plugin"]
        );
        assert_eq!((rows[1].2, rows[1].3), (2, 2));
        let contains = contains_fqns(&s);
        assert!(contains.contains(&(
            "pom.xml.project".into(),
            "pom.xml.project.dependency".into()
        )));
        assert!(contains.contains(&(
            "pom.xml.project".into(),
            "pom.xml.project.dependency-1".into()
        )));
        assert!(contains.contains(&(
            "pom.xml.project.build".into(),
            "pom.xml.project.build.plugin".into()
        )));
    }

    #[test]
    fn dockerfile_stages_and_instructions_are_extracted() {
        let src = "FROM rust:1.98 AS builder\n\
                       RUN cargo build --release\n\
                       COPY . .\n\
                       ENV RUST_LOG=info\n\
                       FROM debian:bookworm\n\
                       RUN apt-get update\n";
        let s = emit(emit_dockerfile, "Dockerfile", src);
        let rows = struct_rows(&s);
        assert_eq!(
            rows.iter().map(|(n, ..)| n.as_str()).collect::<Vec<_>>(),
            vec!["builder", "RUN", "COPY", "ENV", "stage-2", "RUN"]
        );
        let contains = contains_fqns(&s);
        assert!(contains.contains(&("Dockerfile.builder".into(), "Dockerfile.builder.RUN".into())));
        assert!(contains.contains(&(
            "Dockerfile.builder".into(),
            "Dockerfile.builder.COPY".into()
        )));
        assert!(contains.contains(&("Dockerfile.builder".into(), "Dockerfile.builder.ENV".into())));
        assert!(contains.contains(&("Dockerfile.stage-2".into(), "Dockerfile.stage-2.RUN".into())));
    }

    #[test]
    fn makefile_targets_variables_and_includes_are_extracted() {
        let src = "CC := cc\n\
                       CFLAGS = -O2\n\
                       include common.mk\n\
                       all: build test\n\
                       \techo done\n\
                       build:\n\
                       \t$(CC) -o build main.c\n";
        let s = emit(emit_makefile, "Makefile", src);
        assert_eq!(names(&s), vec!["CC", "CFLAGS", "common.mk", "all", "build"]);
        assert!(s.contains.is_empty());
    }

    #[test]
    fn ini_sections_and_keys_are_extracted() {
        let src = "root_key = 1\n\
                       [server]\n\
                       host = localhost\n\
                       port = 8080\n\
                       [client]\n\
                       timeout: 30\n";
        let s = emit(emit_ini, "app.ini", src);
        let rows = struct_rows(&s);
        assert_eq!(
            rows.iter().map(|(n, ..)| n.as_str()).collect::<Vec<_>>(),
            vec!["root_key", "server", "host", "port", "client", "timeout"]
        );
        let contains = contains_fqns(&s);
        assert!(contains.contains(&("app.ini.server".into(), "app.ini.server.host".into())));
        assert!(contains.contains(&("app.ini.client".into(), "app.ini.client.timeout".into())));
    }

    #[test]
    fn md_heading_parser_predicates() {
        let text = "# Title\n\
                        \x20   # indented (4 spaces)\n\
                        ## Sub ##\n\
                        ####### seven\n\
                        ```\n\
                        # fenced\n\
                        ```\n\
                        ### Deep\n";
        let hs = parse_headings(text);
        assert_eq!(
            hs.iter().map(|h| h.level).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert_eq!(
            hs.iter().map(|h| h.text.as_str()).collect::<Vec<_>>(),
            vec!["Title", "Sub", "Deep"]
        );
        assert_eq!(
            hs.iter().map(|h| h.start_line).collect::<Vec<_>>(),
            vec![1, 3, 8]
        );
    }

    #[test]
    fn md_line_count_edge_cases() {
        assert_eq!(line_count(""), 1);
        assert_eq!(line_count("a"), 1);
        assert_eq!(line_count("a\n"), 1);
        assert_eq!(line_count("a\nb"), 2);
        assert_eq!(line_count("a\nb\n"), 2);
    }

    #[test]
    fn claim_and_scope_predicates() {
        // The complete extension→stream routing, including the code-extension
        // skip and the residual `misc`. The skip list is the UNION of every
        // shipped code frontend's extensions: the C++ extras
        // (`.cxx/.c++/.hpp/.hh/.hxx/.tpp/.ipp`) and the unified JS/TS module
        // variants (`.mts/.cts/.mjs/.cjs`) are code-claimed too, while `.c`
        // stays residual `misc` (the C++ frontend does not claim it).
        let code = [
            "rs", "go", "java", "cpp", "cc", "cxx", "c++", "h", "hpp", "hh", "hxx", "tpp", "ipp",
            "ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs", "cs", "py", "pyi",
        ];
        for ext in code {
            assert_eq!(
                stream_for_path(Path::new(&format!("src/file.{ext}"))),
                None,
                "code extension .{ext} must be skipped"
            );
        }
        let routed = [
            ("a.md", Some("md")),
            ("a.markdown", Some("md")),
            ("a.sh", Some("sh")),
            ("a.bash", Some("sh")),
            ("a.yaml", Some("yaml")),
            ("a.yml", Some("yaml")),
            ("a.json", Some("json")),
            ("package-lock.json", Some("json")),
            ("a.toml", Some("toml")),
            ("Cargo.lock", Some("toml")),
            ("a.xml", Some("xml")),
            ("Dockerfile", Some("dockerfile")),
            ("Dockerfile.dev", Some("dockerfile")),
            ("Makefile", Some("makefile")),
            ("GNUmakefile", Some("makefile")),
            ("rules.mk", Some("makefile")),
            ("a.ini", Some("ini")),
            ("a.cfg", Some("ini")),
            (".editorconfig", Some("ini")),
            (".env.local", Some("ini")),
            ("LICENSE", Some("misc")),
            (".gitignore", Some("misc")),
            ("a.c", Some("misc")),
        ];
        for (path, want) in routed {
            assert_eq!(stream_for_path(Path::new(path)), want, "routing for {path}");
        }

        // Pruned trees and the excluded-tree-below-root rule.
        assert!(is_pruned_dir("target"));
        assert!(is_pruned_dir("node_modules"));
        assert!(is_pruned_dir(".git"));
        assert!(is_pruned_dir(".worktrees"));
        assert!(!is_pruned_dir("src"));
        let root = Path::new("/repo");
        assert!(under_excluded_tree(Path::new("/repo/target/a.sh"), root));
        assert!(under_excluded_tree(
            Path::new("/repo/node_modules/x/a.sh"),
            root
        ));
        assert!(!under_excluded_tree(Path::new("/repo/src/a.sh"), root));
        // A scan root that itself lives under `.worktrees/` is still scanned.
        let wt = Path::new("/repo/.worktrees/proj");
        assert!(!under_excluded_tree(wt, wt));

        // `--exclude-path` substrings and the config-scope include/exclude globs.
        let scope = StructuralScope {
            include: vec!["src/**".into()],
            exclude: vec!["src/gen/**".into()],
            code_type: None,
        };
        let base = Path::new("/repo");
        assert!(in_scope(
            Path::new("/repo/src/a.sh"),
            root,
            base,
            &[],
            &[],
            &scope
        ));
        assert!(!in_scope(
            Path::new("/repo/src/gen/a.sh"),
            root,
            base,
            &[],
            &[],
            &scope
        ));
        assert!(!in_scope(
            Path::new("/repo/docs/a.md"),
            root,
            base,
            &[],
            &[],
            &scope
        ));
        assert!(!in_scope(
            Path::new("/repo/src/a.sh"),
            root,
            base,
            &[],
            &["src".into()],
            &scope
        ));
        assert!(!in_scope(
            Path::new("/repo/target/a.sh"),
            root,
            base,
            &[],
            &[],
            &scope
        ));
        let modules = vec![PathBuf::from("/repo/src")];
        assert!(in_scope(
            Path::new("/repo/src/a.sh"),
            root,
            base,
            &modules,
            &[],
            &StructuralScope::default()
        ));
        assert!(!in_scope(
            Path::new("/repo/docs/a.md"),
            root,
            base,
            &modules,
            &[],
            &StructuralScope::default()
        ));

        // glob_match
        assert!(glob_match("**/*.md", "docs/a.md"));
        assert!(glob_match("a?c", "abc"));
        assert!(glob_match("*.md", "a.md"));
        assert!(!glob_match("*.md", "a.txt"));
    }
}

mod int {
    use super::*;

    #[test]
    fn claimed_files_stream_file_and_structure_records() {
        let files: [(&str, &str, &str); 8] = [
            ("build.sh", "sh", "build_all() {\n  echo hi\n}\n"),
            (
                "config.yaml",
                "yaml",
                "name: CI\njobs:\n  build:\n    steps:\n      - run: make\n",
            ),
            ("data.json", "json", "{\n  \"name\": \"x\"\n}\n"),
            ("Cargo.toml", "toml", "[package]\nname = \"x\"\n"),
            (
                "pom.xml",
                "xml",
                "<project>\n  <name>x</name>\n</project>\n",
            ),
            (
                "Dockerfile",
                "dockerfile",
                "FROM rust:1.98 AS builder\nRUN cargo build\n",
            ),
            ("Makefile", "makefile", "all:\n\techo hi\n"),
            ("app.ini", "ini", "[server]\nport = 8080\n"),
        ];
        let recs = assemble(Path::new(""), &files, None);

        // Every File record is well-formed: repo-relative module parent
        // (empty at the repo root) and line 1..line_count.
        for (path, _, text) in files {
            let f = recs
                .iter()
                .find(|r| r["type"] == "file" && r["path"] == path)
                .unwrap_or_else(|| panic!("no File record for {path}"));
            assert_eq!(f["parent"], "");
            assert_eq!(f["start_line"].as_u64(), Some(1));
            assert_eq!(f["end_line"].as_u64(), Some(line_count(text) as u64));
        }

        // Each format's Struct records carry the format's named structure.
        let expected: Vec<(&str, Vec<&str>)> = vec![
            ("build.sh", vec!["build_all"]),
            ("config.yaml", vec!["name", "jobs", "build", "step-1"]),
            ("data.json", vec!["name"]),
            ("Cargo.toml", vec!["package", "name"]),
            ("pom.xml", vec!["project", "name"]),
            ("Dockerfile", vec!["builder", "RUN"]),
            ("Makefile", vec!["all"]),
            ("app.ini", vec!["server", "port"]),
        ];
        for (path, want) in expected {
            let got: Vec<&str> = recs
                .iter()
                .filter(|r| r["type"] == "struct" && r["path"] == path)
                .map(|r| r["name"].as_str().unwrap())
                .collect();
            assert_eq!(got, want, "struct names for {path}");
        }

        // Struct records are complete, and every contains edge references an
        // emitted struct id.
        let ids: std::collections::HashSet<&str> = recs
            .iter()
            .filter(|r| r["type"] == "struct")
            .map(|r| r["id"].as_str().unwrap())
            .collect();
        for r in recs.iter().filter(|r| r["type"] == "struct") {
            for field in [
                "id",
                "parent",
                "name",
                "path",
                "start",
                "end",
                "start_line",
                "end_line",
            ] {
                assert!(r.get(field).is_some(), "struct missing {field}: {r}");
            }
        }
        for r in recs.iter().filter(|r| r["type"] == "contains") {
            assert!(
                ids.contains(r["from"].as_str().unwrap()),
                "dangling from: {r}"
            );
            assert!(ids.contains(r["to"].as_str().unwrap()), "dangling to: {r}");
        }
    }

    #[test]
    fn misc_residual_emits_file_only() {
        let files: [(&str, &str, &str); 2] = [
            ("LICENSE", "misc", "MIT License\n"),
            (".gitignore", "misc", "target/\n"),
        ];
        let recs = assemble(Path::new(""), &files, None);
        let types: Vec<&str> = recs.iter().map(|r| r["type"].as_str().unwrap()).collect();
        // One Module (deduped at the repo root) + two Files, nothing else.
        assert_eq!(types, vec!["module", "file", "file"]);
        assert!(recs
            .iter()
            .all(|r| r["type"] != "struct" && r["type"] != "contains"));
        let paths: Vec<&str> = recs
            .iter()
            .filter(|r| r["type"] == "file")
            .filter_map(|r| r["path"].as_str())
            .collect();
        assert_eq!(paths, vec!["LICENSE", ".gitignore"]);
    }

    #[test]
    fn md_build_doc_fact_set() {
        let base = Path::new("/repo");
        let mut next = 1u64;
        let text = "# Title\n## Sub\n## Sub\n# Title\n";
        let doc = build_doc(
            Path::new("/repo/docs/a.md"),
            text.as_bytes(),
            base,
            "n",
            &mut next,
        );
        assert_eq!(doc.path, "/repo/docs/a.md");
        assert_eq!(doc.dir, "docs");
        assert_eq!(
            doc.sections
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>(),
            vec!["title", "sub", "sub-1", "title-1"]
        );
        // Nesting: the sub-sections hang under the first title, the second
        // title re-roots at the file.
        let structure = md_structure(&doc);
        assert_eq!(names(&structure), vec!["title", "sub", "sub-1", "title-1"]);
        let contains = contains_fqns(&structure);
        assert!(contains.contains(&(
            "/repo/docs/a.md.title".into(),
            "/repo/docs/a.md.title.sub".into()
        )));
        assert!(contains.contains(&(
            "/repo/docs/a.md.title".into(),
            "/repo/docs/a.md.title.sub-1".into()
        )));
        // A repo-root document renders an EMPTY module identity (the ingestor
        // renders it as the bare `md.` root).
        let mut next = 1u64;
        let root_doc = build_doc(Path::new("/repo/README.md"), b"# T\n", base, "n", &mut next);
        assert_eq!(root_doc.dir, "");
    }

    #[test]
    fn walk_claim_stream_wiring() {
        // (a) The claim/scope classifier routes each path to exactly one
        // stream id; a code-extension file is never claimed.
        let routed = [
            ("src/main.rs", None),
            ("src/app.go", None),
            ("src/lib.ts", None),
            ("src/widget.hpp", None),
            ("src/esm.mjs", None),
            ("README.md", Some("md")),
            ("scripts/build.sh", Some("sh")),
            ("ci.yml", Some("yaml")),
            ("package.json", Some("json")),
            ("Cargo.toml", Some("toml")),
            ("pom.xml", Some("xml")),
            ("Dockerfile", Some("dockerfile")),
            ("Makefile", Some("makefile")),
            ("app.ini", Some("ini")),
            ("LICENSE", Some("misc")),
            (".gitignore", Some("misc")),
            ("Cargo.lock", Some("toml")),
            ("package-lock.json", Some("json")),
        ];
        for (path, want) in routed {
            assert_eq!(stream_for_path(Path::new(path)), want, "routing for {path}");
        }

        // The config scope narrows the claim and a pruned tree is never claimed.
        let scope = StructuralScope {
            include: vec!["src/**".into()],
            exclude: vec!["src/gen/**".into()],
            code_type: None,
        };
        let root = Path::new("/repo");
        assert!(in_scope(
            Path::new("/repo/src/a.sh"),
            root,
            root,
            &[],
            &[],
            &scope
        ));
        assert!(!in_scope(
            Path::new("/repo/target/a.sh"),
            root,
            root,
            &[],
            &[],
            &scope
        ));
        assert!(!in_scope(
            Path::new("/repo/src/gen/a.sh"),
            root,
            root,
            &[],
            &[],
            &scope
        ));

        // (b) A per-stream `--stream <id>` selection emits only that stream's
        // records; the full walk emits the union.
        let files: [(&str, &str, &str); 3] = [
            ("a.sh", "sh", "foo() { :; }\n"),
            ("b.yaml", "yaml", "key: 1\n"),
            ("c.ini", "ini", "[s]\nk = 1\n"),
        ];
        let full = assemble(root, &files, None);
        let sh_only = assemble(root, &files, Some("sh"));
        let full_files: Vec<&str> = full
            .iter()
            .filter(|r| r["type"] == "file")
            .filter_map(|r| r["path"].as_str())
            .collect();
        assert_eq!(full_files, vec!["a.sh", "b.yaml", "c.ini"]);
        let sh_files: Vec<&str> = sh_only
            .iter()
            .filter(|r| r["type"] == "file")
            .filter_map(|r| r["path"].as_str())
            .collect();
        assert_eq!(sh_files, vec!["a.sh"]);
        assert!(sh_only
            .iter()
            .all(|r| r["type"] != "file" || r["path"] == "a.sh"));
        // The union's struct set is the per-stream sets concatenated.
        assert_eq!(full.iter().filter(|r| r["type"] == "struct").count(), 4);
        assert_eq!(sh_only.iter().filter(|r| r["type"] == "struct").count(), 1);
    }
}
