mod graph;

use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use graph::*;
use lbug::{Connection, Database};

fn is_blacklisted(fqn: &str, blacklist: &[String]) -> bool {
    blacklist.iter().any(|p| fqn.starts_with(p.as_str()))
}

fn main() {
    let raw: Vec<String> = std::env::args().collect();
    let mut clean = vec![raw[0].clone()];
    let mut path_excludes: Vec<String> = Vec::new();
    let mut i = 1;
    while i < raw.len() {
        if raw[i] == "--exclude-path" {
            i += 1;
            if i < raw.len() {
                path_excludes.push(raw[i].clone());
            }
        } else {
            clean.push(raw[i].clone());
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

    let blacklist: Vec<String> = clean.iter().skip(2).cloned().collect();
    if !blacklist.is_empty() {
        eprintln!("Blacklist: {:?}", blacklist);
    }
    if !path_excludes.is_empty() {
        eprintln!("Path excludes: {:?}", path_excludes);
    }

    let mut java_args = format!("\"{}\"", project_dir.display());
    for pat in &path_excludes {
        java_args.push_str(&format!(" \"{}\"", pat));
    }
    let mut frontend_output = Command::new("sh")
        .args([
            "-c",
            &format!("{} {}", env!("APG_FRONTEND_CMD"), java_args),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("Failed to run apg frontend");

    let mut graph = Graph::default();
    let mut skipped = 0u64;

    for line in BufReader::new(frontend_output.stdout.as_mut().unwrap())
        .lines()
        .map(|x| x.expect("io error"))
    {
        let msg: serde_json::Value = serde_json::from_str(&line).unwrap_or_else(|_| panic!("bad json: {line}"));
        match msg["type"].as_str().unwrap() {
            "pkg" => {
                let fqn = msg["fqn"].as_str().unwrap();
                if is_blacklisted(fqn, &blacklist) {
                    skipped += 1;
                    continue;
                }
                graph.nodes.insert(
                    fqn.to_owned(),
                    Node {
                        kind: NodeKind::Module,
                        location: None,
                    },
                );
            }
            "decl" => {
                let fqn = msg["fqn"].as_str().unwrap();
                if is_blacklisted(fqn, &blacklist) {
                    skipped += 1;
                    continue;
                }
                graph.nodes.insert(
                    fqn.to_owned(),
                    Node {
                        kind: match msg["kind"].as_str().unwrap() {
                            "class" => NodeKind::Struct,
                            "method" => NodeKind::Function,
                            x => panic!("invalid node kind {x}"),
                        },
                        location: Some(Location {
                            path: PathBuf::from(msg["path"].as_str().unwrap()),
                            start: msg["start"].as_u64().unwrap() as u32,
                            end: msg["end"].as_u64().unwrap() as u32,
                        }),
                    },
                );
            }
            "contains" => {
                let parent = msg["parent"].as_str().unwrap();
                let child = msg["child"].as_str().unwrap();
                if is_blacklisted(parent, &blacklist) || is_blacklisted(child, &blacklist) {
                    skipped += 1;
                    continue;
                }
                graph.contains.insert((
                    parent.to_owned(),
                    child.to_owned(),
                ));
            }
            "call" => {
                let source = msg["source"].as_str().unwrap();
                let target = msg["target"].as_str().unwrap();
                if is_blacklisted(source, &blacklist) || is_blacklisted(target, &blacklist) {
                    skipped += 1;
                    continue;
                }
                graph.calls.insert((
                    source.to_owned(),
                    target.to_owned(),
                ));
            }
            "use" => {
                let source = msg["source"].as_str().unwrap();
                let target = msg["target"].as_str().unwrap();
                if is_blacklisted(source, &blacklist) || is_blacklisted(target, &blacklist) {
                    skipped += 1;
                    continue;
                }
                graph.uses.insert((
                    source.to_owned(),
                    target.to_owned(),
                ));
            }
            x => panic!("invalid msg type {x}"),
        }
    }

    eprintln!("Skipped {skipped} blacklisted messages");

    if !frontend_output
        .wait()
        .expect("couldnt wait for apg frontend")
        .success()
    {
        panic!("apg frontend failed");
    }

    dbg!(graph.uses.len());
    graph.uses.retain(|pair| graph.nodes.contains_key(&pair.0) && graph.nodes.contains_key(&pair.1) && graph.nodes[&pair.1].kind == NodeKind::Struct);

    eprintln!(
        "graph: {} nodes, {} contain edges, {} calls edges, {} uses edges",
        graph.nodes.len(),
        graph.contains.len(),
        graph.calls.len(),
        graph.uses.len(),
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
            `end` INT64
        )",
    )
    .unwrap();
    conn.query(
        "
        CREATE NODE TABLE Function(
            fqn STRING PRIMARY KEY,
            path STRING,
            start INT64,
            `end` INT64
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
        module_csv.write_all(b"fqn\n").unwrap();
        struct_csv.write_all(b"fqn,path,start,end\n").unwrap();
        function_csv.write_all(b"fqn,path,start,end\n").unwrap();

        for (fqn, node) in &graph.nodes {
            match (node.kind, &node.location) {
                (NodeKind::Module, _) => {
                    module_csv.write_all(fqn.as_bytes()).unwrap();
                    module_csv.write_all(b"\n").unwrap();
                }
                (NodeKind::Struct, Some(loc)) => struct_csv
                    .write_all(
                        format!(
                            "{},{},{},{}\n",
                            fqn,
                            loc.path.to_str().unwrap(),
                            loc.start,
                            loc.end
                        )
                        .as_bytes(),
                    )
                    .unwrap(),
                (NodeKind::Function, Some(loc)) => function_csv
                    .write_all(
                        format!(
                            "{},{},{},{}\n",
                            fqn,
                            loc.path.to_str().unwrap(),
                            loc.start,
                            loc.end
                        )
                        .as_bytes(),
                    )
                    .unwrap(),
                (_, _) => panic!(),
            }
        }
    }

    conn.query(
        r#"
        COPY Module FROM "modules.csv" (header=true);
        COPY Struct FROM "structs.csv" (header=true);
        COPY Function FROM "functions.csv" (header=true);
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
    }

    let query = r#"COPY Contains FROM "contains_mod_mod.csv" (from="Module",to="Module");
        COPY Contains FROM "contains_mod_struct.csv" (from="Module",to="Struct");
        COPY Contains FROM "contains_mod_fn.csv" (from="Module",to="Function");
        COPY Contains FROM "contains_struct_struct.csv" (from="Struct",to="Struct");
        COPY Contains FROM "contains_struct_fn.csv" (from="Struct",to="Function");
        COPY Calls FROM "calls.csv";
        COPY Uses FROM "uses_struct.csv" (from="Struct",to="Struct");
        COPY Uses FROM "uses_fn.csv" (from="Function",to="Struct");"#;
    for line in query.lines() {
        println!("{:?}", conn.query(line).unwrap());
    }
}
