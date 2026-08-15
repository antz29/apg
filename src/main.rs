mod classify;
mod cleanup;
mod graph;

use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use cleanup::{cleanup, CleanupOptions};
use graph::*;
use lbug::{Connection, Database};

fn is_blacklisted(fqn: &str, blacklist: &[String]) -> bool {
    blacklist.iter().any(|p| fqn.starts_with(p.as_str()))
}

/// Inserts an UnresolvedTarget node, setting its category on first write. If
/// the node already exists, its existing category is preserved (categories are
/// structurally disjoint per FQN, so a later conflict is treated as a no-op).
fn insert_unresolved_target(graph: &mut Graph, fqn: &str, category: Option<&str>) {
    graph.nodes.entry(fqn.to_owned()).or_insert_with(|| Node {
        kind: NodeKind::UnresolvedTarget,
        location: None,
        category: category.map(str::to_owned),
        code_type: String::new(),
    });
}

fn available_languages() -> Vec<String> {
    env!("APG_LANGUAGES")
        .split(',')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

fn has_extension(dir: &std::path::Path, exts: &[&str], depth: u32) -> bool {
    if depth == 0 {
        return false;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return false,
    };
    for entry in entries.flatten() {
        let p = entry.path();
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name.starts_with('.') || name == "target" || name == "node_modules" {
            continue;
        }
        if p.is_dir() {
            if has_extension(&p, exts, depth - 1) {
                return true;
            }
        } else if p.extension().map_or(false, |e| exts.contains(&format!(".{}", e.to_str().unwrap_or("")).as_str())) {
            return true;
        }
    }
    false
}

fn auto_detect_language(dir: &std::path::Path, available: &[String]) -> Option<String> {
    let candidates: Vec<(&str, &[&str])> = vec![
        ("java", &[".java"] as &[&str]),
        ("go", &[".go"]),
        ("cpp", &[".cpp", ".cc", ".cxx", ".hpp", ".h", ".hh"]),
    ];

    for (lang, exts) in &candidates {
        if available.iter().any(|l| l == lang) && has_extension(dir, exts, 5) {
            return Some(lang.to_string());
        }
    }
    None
}

fn frontend_cmd(language: &str) -> String {
    match language {
        "cpp" => env!("APG_FRONTEND_CPP").to_string(),
        "go" => env!("APG_FRONTEND_GO").to_string(),
        "java" => env!("APG_FRONTEND_JAVA").to_string(),
        _ => panic!("unknown language: {language}"),
    }
}

fn main() {
    let raw: Vec<String> = std::env::args().collect();
    let mut clean = vec![raw[0].clone()];
    let mut language: Option<String> = None;
    let mut path_excludes: Vec<String> = Vec::new();
    let mut module_dirs: Vec<String> = Vec::new();
    let mut i = 1;
    while i < raw.len() {
        match raw[i].as_str() {
            "--language" | "-l" => {
                i += 1;
                if i < raw.len() {
                    language = Some(raw[i].clone());
                }
            }
            "--exclude-path" => {
                i += 1;
                if i < raw.len() {
                    path_excludes.push(raw[i].clone());
                }
            }
            "--module" => {
                i += 1;
                if i < raw.len() {
                    module_dirs.push(raw[i].clone());
                }
            }
            _ => clean.push(raw[i].clone()),
        }
        i += 1;
    }

    let project_dir = if clean.len() > 1 {
        PathBuf::from(&clean[1])
    } else {
        PathBuf::from("project")
    };
    let project_dir = project_dir.canonicalize().unwrap();
    eprintln!("Project: {}", project_dir.display());

    let available = available_languages();
    if available.is_empty() {
        panic!("No language frontends compiled. Install gcc, go, or javac and rebuild.");
    }

    let language = language.unwrap_or_else(|| {
        auto_detect_language(&project_dir, &available)
            .unwrap_or_else(|| available[0].clone())
    });

    if !available.iter().any(|l| l == &language) {
        panic!(
            "Language '{language}' is not available. Built with: {}. Install the required toolchain and rebuild.",
            available.join(", ")
        );
    }
    eprintln!("Language: {language}");

    let blacklist: Vec<String> = clean.iter().skip(2).cloned().collect();
    if !blacklist.is_empty() {
        eprintln!("Blacklist: {:?}", blacklist);
    }
    if !path_excludes.is_empty() {
        eprintln!("Path excludes: {:?}", path_excludes);
    }

    let cmd = frontend_cmd(&language);
    let config = classify::ApgConfig::load(&project_dir);

    if language == "cpp" || language == "go" {
        let mut cmd = Command::new(&cmd);
        cmd.arg(project_dir.display().to_string());
        for m in &module_dirs {
            cmd.arg("--module").arg(m);
        }
        cmd.args(&path_excludes)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        let mut frontend_output = cmd.spawn().expect("Failed to run frontend");

        build_graph(&mut frontend_output, &blacklist, &path_excludes, &language, config.as_ref());

        if !frontend_output
            .wait()
            .expect("couldn't wait for frontend")
            .success()
        {
            panic!("{language} frontend failed");
        }
    } else {
        let mut args = format!("\"{}\"", project_dir.display());
        for m in &module_dirs {
            args.push_str(&format!(" --module \"{}\"", m));
        }
        for pat in &path_excludes {
            args.push_str(&format!(" \"{}\"", pat));
        }
        let mut frontend_output = Command::new("sh")
            .args(["-c", &format!("{} {}", cmd, args)])
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("Failed to run Java frontend");

        build_graph(&mut frontend_output, &blacklist, &path_excludes, &language, config.as_ref());

        if !frontend_output
            .wait()
            .expect("couldn't wait for Java frontend")
            .success()
        {
            panic!("java frontend failed");
        }
    }
}

fn build_graph(
    frontend_output: &mut std::process::Child,
    blacklist: &[String],
    path_excludes: &[String],
    language: &str,
    config: Option<&classify::ApgConfig>,
) {
    let mut graph = Graph::default();
    let mut skipped = 0u64;

    for line in BufReader::new(frontend_output.stdout.as_mut().unwrap())
        .lines()
        .map(|x| x.expect("io error"))
    {
        let msg: serde_json::Value =
            serde_json::from_str(&line).unwrap_or_else(|_| panic!("bad json: {line}"));
        match msg["type"].as_str().unwrap() {
            "pkg" => {
                let fqn = msg["fqn"].as_str().unwrap();
                if is_blacklisted(fqn, blacklist) {
                    skipped += 1;
                    continue;
                }
                graph.nodes.insert(
                    fqn.to_owned(),
                    Node {
                        kind: NodeKind::Module,
                        location: None,
                        category: None,
                        code_type: String::new(),
                    },
                );
            }
            "decl" => {
                let fqn = msg["fqn"].as_str().unwrap();
                if is_blacklisted(fqn, blacklist) {
                    skipped += 1;
                    continue;
                }
                let path = msg["path"].as_str().unwrap();
                graph.nodes.insert(
                    fqn.to_owned(),
                    Node {
                        kind: match msg["kind"].as_str().unwrap() {
                            "class" => NodeKind::Struct,
                            "method" => NodeKind::Function,
                            x => panic!("invalid node kind {x}"),
                        },
                        location: Some(Location {
                            path: PathBuf::from(path),
                            start: msg["start"].as_u64().unwrap_or(0) as u32,
                            end: msg["end"].as_u64().unwrap_or(0) as u32,
                        }),
                        category: None,
                        code_type: classify::classify_code_type(path, fqn, language, config),
                    },
                );
            }
            "contains" => {
                let parent = msg["parent"].as_str().unwrap();
                let child = msg["child"].as_str().unwrap();
                if is_blacklisted(parent, blacklist) || is_blacklisted(child, blacklist) {
                    skipped += 1;
                    continue;
                }
                graph
                    .contains
                    .insert((parent.to_owned(), child.to_owned()));
            }
            "call" => {
                let source = msg["source"].as_str().unwrap();
                let target = msg["target"].as_str().unwrap();
                if is_blacklisted(source, blacklist) || is_blacklisted(target, blacklist) {
                    skipped += 1;
                    continue;
                }
                graph
                    .calls
                    .insert((source.to_owned(), target.to_owned()));
            }
            "use" => {
                let source = msg["source"].as_str().unwrap();
                let target = msg["target"].as_str().unwrap();
                if is_blacklisted(source, blacklist) || is_blacklisted(target, blacklist) {
                    skipped += 1;
                    continue;
                }
                graph
                    .uses
                    .insert((source.to_owned(), target.to_owned()));
            }
            "u_call" => {
                let source = msg["source"].as_str().unwrap();
                let target = msg["target"].as_str().unwrap();
                if is_blacklisted(source, blacklist) {
                    skipped += 1;
                    continue;
                }
                let category = msg
                    .get("category")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty());
                let target_type = msg
                    .get("target_type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                insert_unresolved_target(&mut graph, target, category);
                graph.unresolved_calls.insert((
                    source.to_owned(),
                    target.to_owned(),
                    target_type.to_owned(),
                ));
            }
            "u_use" => {
                let source = msg["source"].as_str().unwrap();
                let target = msg["target"].as_str().unwrap();
                if is_blacklisted(source, blacklist) {
                    skipped += 1;
                    continue;
                }
                let category = msg
                    .get("category")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty());
                insert_unresolved_target(&mut graph, target, category);
                graph
                    .unresolved_uses
                    .insert((source.to_owned(), target.to_owned()));
            }
            x => panic!("invalid msg type {x}"),
        }
    }

    eprintln!("Skipped {skipped} blacklisted messages");

    graph.contains.retain(|(a, b)| {
        graph.nodes.contains_key(a)
            && graph.nodes.contains_key(b)
            && matches!(
                (graph.nodes[a].kind, graph.nodes[b].kind),
                (NodeKind::Module, NodeKind::Module)
                    | (NodeKind::Module, NodeKind::Struct)
                    | (NodeKind::Module, NodeKind::Function)
                    | (NodeKind::Struct, NodeKind::Struct)
                    | (NodeKind::Struct, NodeKind::Function)
            )
    });
    graph.calls.retain(|(a, b)| {
        graph.nodes.contains_key(a)
            && graph.nodes.contains_key(b)
            && graph.nodes[a].kind == NodeKind::Function
            && graph.nodes[b].kind == NodeKind::Function
    });
    graph.uses.retain(|(a, b)| {
        graph.nodes.contains_key(a)
            && graph.nodes.contains_key(b)
            && graph.nodes[b].kind == NodeKind::Struct
            && matches!(graph.nodes[a].kind, NodeKind::Function | NodeKind::Struct)
    });
    graph.unresolved_calls.retain(|(a, b, _)| {
        graph.nodes.contains_key(a)
            && graph.nodes.contains_key(b)
            && graph.nodes[a].kind == NodeKind::Function
            && graph.nodes[b].kind == NodeKind::UnresolvedTarget
    });
    graph.unresolved_uses.retain(|(a, b)| {
        graph.nodes.contains_key(a)
            && graph.nodes.contains_key(b)
            && graph.nodes[b].kind == NodeKind::UnresolvedTarget
            && matches!(graph.nodes[a].kind, NodeKind::Function | NodeKind::Struct)
    });

    eprintln!(
        "graph: {} nodes, {} contain edges, {} calls edges, {} uses edges, {} unresolved calls, {} unresolved uses",
        graph.nodes.len(),
        graph.contains.len(),
        graph.calls.len(),
        graph.uses.len(),
        graph.unresolved_calls.len(),
        graph.unresolved_uses.len(),
    );

    let report = cleanup(
        &mut graph,
        &CleanupOptions {
            user_excludes: path_excludes.to_vec(),
            language: language.to_string(),
        },
    );
    eprintln!(
        "cleanup: removed {} nodes, {} contains, {} calls, {} uses, {} unresolved calls, {} unresolved uses, {} span violations",
        report.nodes_removed,
        report.contains_removed,
        report.calls_removed,
        report.uses_removed,
        report.unresolved_calls_removed,
        report.unresolved_uses_removed,
        report.span_violations_removed,
    );

    let _ = std::fs::remove_file("db.lbug");
    let db = Database::new("db.lbug", Default::default()).unwrap();
    let conn = Connection::new(&db).unwrap();
    conn.query(
        "
        CREATE NODE TABLE Module(
            fqn STRING PRIMARY KEY
        )",
    )
    .unwrap();
    conn.query(
        "
        CREATE NODE TABLE Struct(
            fqn STRING PRIMARY KEY,
            path STRING,
            start INT64,
            `end` INT64,
            code_type STRING
        )",
    )
    .unwrap();
    conn.query(
        "
        CREATE NODE TABLE Function(
            fqn STRING PRIMARY KEY,
            path STRING,
            start INT64,
            `end` INT64,
            code_type STRING
        )",
    )
    .unwrap();
    conn.query(
        "
        CREATE NODE TABLE UnresolvedTarget(
            fqn STRING PRIMARY KEY,
            category STRING
        )",
    )
    .unwrap();
    conn.query(
        "
        CREATE REL TABLE Contains(
            FROM Module TO Module,
            FROM Module TO Struct,
            FROM Module TO Function,
            FROM Struct TO Struct,
            FROM Struct TO Function
        )",
    )
    .unwrap();
    conn.query(
        "
        CREATE REL TABLE Calls(
            FROM Function TO Function
        )",
    )
    .unwrap();
    conn.query(
        "
        CREATE REL TABLE Uses(
            FROM Function TO Struct,
            FROM Struct TO Struct
        )",
    )
    .unwrap();
    conn.query(
        "
        CREATE REL TABLE UnresolvedCall(
            FROM Function TO UnresolvedTarget,
            target_type STRING
        )",
    )
    .unwrap();
    conn.query(
        "
        CREATE REL TABLE UnresolvedUse(
            FROM Function TO UnresolvedTarget,
            FROM Struct TO UnresolvedTarget
        )",
    )
    .unwrap();

    {
        let mut module_csv = BufWriter::new(
            File::options()
                .write(true)
                .create(true)
                .truncate(true)
                .open("modules.csv")
                .unwrap(),
        );
        let mut struct_csv = BufWriter::new(
            File::options()
                .write(true)
                .create(true)
                .truncate(true)
                .open("structs.csv")
                .unwrap(),
        );
        let mut function_csv = BufWriter::new(
            File::options()
                .write(true)
                .create(true)
                .truncate(true)
                .open("functions.csv")
                .unwrap(),
        );
        let mut unresolved_csv = BufWriter::new(
            File::options()
                .write(true)
                .create(true)
                .truncate(true)
                .open("unresolved.csv")
                .unwrap(),
        );
        module_csv.write_all(b"fqn\n").unwrap();
        struct_csv.write_all(b"fqn,path,start,end,code_type\n").unwrap();
        function_csv.write_all(b"fqn,path,start,end,code_type\n").unwrap();
        unresolved_csv.write_all(b"fqn,category\n").unwrap();

        for (fqn, node) in &graph.nodes {
            match (node.kind, &node.location) {
                (NodeKind::Module, _) => {
                    module_csv.write_all(fqn.as_bytes()).unwrap();
                    module_csv.write_all(b"\n").unwrap();
                }
                (NodeKind::Struct, Some(loc)) => struct_csv
                    .write_all(
                        format!(
                            "{},{},{},{},{}\n",
                            fqn,
                            loc.path.to_str().unwrap(),
                            loc.start,
                            loc.end,
                            node.code_type
                        )
                        .as_bytes(),
                    )
                    .unwrap(),
                (NodeKind::Function, Some(loc)) => function_csv
                    .write_all(
                        format!(
                            "{},{},{},{},{}\n",
                            fqn,
                            loc.path.to_str().unwrap(),
                            loc.start,
                            loc.end,
                            node.code_type
                        )
                        .as_bytes(),
                    )
                    .unwrap(),
                (NodeKind::UnresolvedTarget, None) => {
                    unresolved_csv.write_all(fqn.as_bytes()).unwrap();
                    unresolved_csv.write_all(b",").unwrap();
                    if let Some(cat) = &node.category {
                        unresolved_csv.write_all(cat.as_bytes()).unwrap();
                    }
                    unresolved_csv.write_all(b"\n").unwrap();
                }
                (_, _) => panic!(),
            }
        }
    }

    conn.query(
        r#"
        COPY Module FROM "modules.csv" (header=true);
        COPY Struct FROM "structs.csv" (header=true);
        COPY Function FROM "functions.csv" (header=true);
        COPY UnresolvedTarget FROM "unresolved.csv" (header=true);
        "#,
    )
    .unwrap();

    {
        let mut edges_csv = BufWriter::new(
            File::options()
                .write(true)
                .create(true)
                .truncate(true)
                .open("edges.csv")
                .unwrap(),
        );
        edges_csv.write_all(b"from,to\n").unwrap();

        let mut contain_mod_mod_csv = BufWriter::new(
            File::options()
                .write(true)
                .create(true)
                .truncate(true)
                .open("contains_mod_mod.csv")
                .unwrap(),
        );
        let mut contain_mod_struct_csv = BufWriter::new(
            File::options()
                .write(true)
                .create(true)
                .truncate(true)
                .open("contains_mod_struct.csv")
                .unwrap(),
        );
        let mut contain_mod_fn_csv = BufWriter::new(
            File::options()
                .write(true)
                .create(true)
                .truncate(true)
                .open("contains_mod_fn.csv")
                .unwrap(),
        );
        let mut contain_struct_struct_csv = BufWriter::new(
            File::options()
                .write(true)
                .create(true)
                .truncate(true)
                .open("contains_struct_struct.csv")
                .unwrap(),
        );
        let mut contain_struct_fn_csv = BufWriter::new(
            File::options()
                .write(true)
                .create(true)
                .truncate(true)
                .open("contains_struct_fn.csv")
                .unwrap(),
        );
        let mut call_csv = BufWriter::new(
            File::options()
                .write(true)
                .create(true)
                .truncate(true)
                .open("calls.csv")
                .unwrap(),
        );
        let mut use_struct_csv = BufWriter::new(
            File::options()
                .write(true)
                .create(true)
                .truncate(true)
                .open("uses_struct.csv")
                .unwrap(),
        );
        let mut use_fn_csv = BufWriter::new(
            File::options()
                .write(true)
                .create(true)
                .truncate(true)
                .open("uses_fn.csv")
                .unwrap(),
        );
        let mut unresolved_call_csv = BufWriter::new(
            File::options()
                .write(true)
                .create(true)
                .truncate(true)
                .open("unresolved_call.csv")
                .unwrap(),
        );
        let mut unresolved_use_fn_csv = BufWriter::new(
            File::options()
                .write(true)
                .create(true)
                .truncate(true)
                .open("unresolved_use_fn.csv")
                .unwrap(),
        );
        let mut unresolved_use_struct_csv = BufWriter::new(
            File::options()
                .write(true)
                .create(true)
                .truncate(true)
                .open("unresolved_use_struct.csv")
                .unwrap(),
        );
        unresolved_call_csv.write_all(b"from,to,target_type\n").unwrap();
        unresolved_use_fn_csv.write_all(b"from,to\n").unwrap();
        unresolved_use_struct_csv.write_all(b"from,to\n").unwrap();

        for (a, b) in graph.contains {
            match (graph.nodes[&a].kind, graph.nodes[&b].kind) {
                (NodeKind::Module, NodeKind::Module) => contain_mod_mod_csv.write_all(format!("{a},{b}\n").as_bytes()).unwrap(),
                (NodeKind::Module, NodeKind::Struct) => contain_mod_struct_csv.write_all(format!("{a},{b}\n").as_bytes()).unwrap(),
                (NodeKind::Module, NodeKind::Function) => contain_mod_fn_csv.write_all(format!("{a},{b}\n").as_bytes()).unwrap(),
                (NodeKind::Struct, NodeKind::Struct) => contain_struct_struct_csv.write_all(format!("{a},{b}\n").as_bytes()).unwrap(),
                (NodeKind::Struct, NodeKind::Function) => contain_struct_fn_csv.write_all(format!("{a},{b}\n").as_bytes()).unwrap(),
                (x, y) => println!("{a} {b} {x:?}{y:?}"),
            }
            edges_csv.write_fmt(format_args!("{a},{b}\n")).unwrap();
        }
        for (a, b) in graph.calls {
            call_csv.write_all(format!("{a},{b}\n").as_bytes()).unwrap();
            edges_csv.write_fmt(format_args!("{a},{b}\n")).unwrap();
        }
        for (a, b) in graph.uses {
            match graph.nodes[&a].kind {
                NodeKind::Struct => use_struct_csv.write_all(format!("{a},{b}\n").as_bytes()).unwrap(),
                NodeKind::Function=> use_fn_csv.write_all(format!("{a},{b}\n").as_bytes()).unwrap(),
                x  => unreachable!("{a} {b} {x:?}"),
            }
            edges_csv.write_fmt(format_args!("{a},{b}\n")).unwrap();
        }
        for (a, b, t) in graph.unresolved_calls {
            unresolved_call_csv
                .write_all(format!("{a},{b},{t}\n").as_bytes())
                .unwrap();
            edges_csv.write_fmt(format_args!("{a},{b}\n")).unwrap();
        }
        for (a, b) in graph.unresolved_uses {
            match graph.nodes[&a].kind {
                NodeKind::Function => unresolved_use_fn_csv.write_all(format!("{a},{b}\n").as_bytes()).unwrap(),
                NodeKind::Struct => unresolved_use_struct_csv.write_all(format!("{a},{b}\n").as_bytes()).unwrap(),
                x => unreachable!("{a} {b} {x:?}"),
            }
            edges_csv.write_fmt(format_args!("{a},{b}\n")).unwrap();
        }
    }

    let query = r#"COPY Contains FROM "contains_mod_mod.csv" (header=true, from="Module",to="Module");
        COPY Contains FROM "contains_mod_struct.csv" (header=true, from="Module",to="Struct");
        COPY Contains FROM "contains_mod_fn.csv" (header=true, from="Module",to="Function");
        COPY Contains FROM "contains_struct_struct.csv" (header=true, from="Struct",to="Struct");
        COPY Contains FROM "contains_struct_fn.csv" (header=true, from="Struct",to="Function");
        COPY Calls FROM "calls.csv" (header=true);
        COPY Uses FROM "uses_struct.csv" (header=true, from="Struct",to="Struct");
        COPY Uses FROM "uses_fn.csv" (header=true, from="Function",to="Struct");
        COPY UnresolvedCall FROM "unresolved_call.csv" (header=true);
        COPY UnresolvedUse FROM "unresolved_use_fn.csv" (header=true, from="Function",to="UnresolvedTarget");
        COPY UnresolvedUse FROM "unresolved_use_struct.csv" (header=true, from="Struct",to="UnresolvedTarget");"#;
    for line in query.lines() {
        conn.query(line).unwrap();
    }
}
