mod graph;

use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use graph::*;
use lbug::{Connection, Database};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let project_dir = if args.len() > 1 {
        PathBuf::from(&args[1])
    } else {
        PathBuf::from("project")
    };
    let project_dir = project_dir.canonicalize().unwrap();
    eprintln!("Project: {}", project_dir.display());

    let mut frontend_output = Command::new("sh")
        .args([
            "-c",
            &format!("{} \"{}\"", env!("APG_FRONTEND_CMD"), project_dir.display()),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("Failed to run apg frontend");

    let mut graph = Graph::default();

    for line in BufReader::new(frontend_output.stdout.as_mut().unwrap())
        .lines()
        .map(|x| x.expect("io error"))
    {
        let msg: serde_json::Value = serde_json::from_str(&line).unwrap_or_else(|_| panic!("bad json: {line}"));
        match msg["type"].as_str().unwrap() {
            "pkg" => {
                graph.nodes.insert(
                    msg["fqn"].as_str().unwrap().to_owned(),
                    Node {
                        kind: NodeKind::Module,
                        location: None,
                    },
                );
            }
            "decl" => {
                graph.nodes.insert(
                    msg["fqn"].as_str().unwrap().to_owned(),
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
                graph.contains.insert((
                    msg["parent"].as_str().unwrap().to_owned(),
                    msg["child"].as_str().unwrap().to_owned(),
                ));
            }
            "call" => {
                graph.calls.insert((
                    msg["source"].as_str().unwrap().to_owned(),
                    msg["target"].as_str().unwrap().to_owned(),
                ));
            }
            "use" => {
                graph.uses.insert((
                    msg["source"].as_str().unwrap().to_owned(),
                    msg["target"].as_str().unwrap().to_owned(),
                ));
            }
            x => panic!("invalid msg type {x}"),
        }
    }

    if !frontend_output
        .wait()
        .expect("couldnt wait for apg frontend")
        .success()
    {
        let mut buf = String::new();
        frontend_output
            .stderr
            .unwrap()
            .read_to_string(&mut buf)
            .unwrap();
        panic!("apg frontend failed:\n{buf}");
    }

    eprintln!(
        "graph: {} nodes, {} contain edges, {} calls edges, {} uses edges",
        graph.nodes.len(),
        graph.contains.len(),
        graph.calls.len(),
        graph.uses.len(),
    );

    for (fqn, node) in graph.nodes.iter().take(10) {
        println!("{}: {:?}", fqn, node.kind);
        if node.location.is_none() {
            continue;
        }
        println!(
            "{}\n",
            String::from_utf8_lossy(
                &std::fs::read(&node.location.as_ref().unwrap().path).unwrap()[node
                    .location
                    .as_ref()
                    .unwrap()
                    .start
                    as usize
                    ..node.location.as_ref().unwrap().end as usize]
            )
        );
    }

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
                (x, y) => unreachable!("{a} {b} {x:?}{y:?}"),
            }
        }
        for (a, b) in graph.calls {
            call_csv.write_all(format!("{a},{b}\n").as_bytes()).unwrap();
        }
        for (a, b) in graph.uses {
            match graph.nodes[&a].kind {
                NodeKind::Struct => use_struct_csv.write_all(format!("{a},{b}\n").as_bytes()).unwrap(),
                NodeKind::Function=> use_fn_csv.write_all(format!("{a},{b}\n").as_bytes()).unwrap(),
                _ => unreachable!(),
            }
        }
    }

    conn.query(
        r#"
        COPY Contains FROM "contains_mod_mod.csv" (from="Module",to="Module");
        COPY Contains FROM "contains_mod_struct.csv" (from="Module",to="Struct");
        COPY Contains FROM "contains_mod_fn.csv" (from="Module",to="Function");
        COPY Contains FROM "contains_struct_struct.csv" (from="Struct",to="Struct");
        COPY Contains FROM "contains_struct_fn.csv" (from="Struct",to="Function");
        COPY Calls FROM "calls.csv" (from="Function",to="Function");
        COPY Uses FROM "uses_struct.csv" (from="Struct",to="Struct");
        COPY Uses FROM "uses_fn.csv" (from="Function",to="Struct");
        "#,
    )
    .unwrap();
}
