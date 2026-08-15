mod classify;
mod cleanup;
mod graph;
mod ingest;
mod load;
mod schema;

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use cleanup::{cleanup, CleanupOptions};
use lbug::{Connection, Database};

/// Mirrors every run message to stderr *and* to `apg-frontend.log`, so the log
/// is a complete high-resolution record of the run (the frontend's stderr is
/// also redirected there, so one file has the whole pipeline).
struct Log {
    f: std::fs::File,
}

impl Log {
    fn new() -> Log {
        Log {
            f: std::fs::File::create("apg-frontend.log")
                .expect("failed to create apg-frontend.log"),
        }
    }

    fn ln(&mut self, msg: &str) {
        eprintln!("{msg}");
        let _ = writeln!(self.f, "{msg}");
    }
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
        } else if p
            .extension()
            .map_or(false, |e| exts.contains(&format!(".{}", e.to_str().unwrap_or("")).as_str()))
        {
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

fn temp_dir() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("apg-load-{}-{nanos}", std::process::id()))
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

    let mut log = Log::new();
    log.ln(&format!("Project: {}", project_dir.display()));

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
    log.ln(&format!("Language: {language}"));

    let blacklist: Vec<String> = clean.iter().skip(2).cloned().collect();
    if !blacklist.is_empty() {
        log.ln(&format!("Blacklist: {:?}", blacklist));
    }
    if !path_excludes.is_empty() {
        log.ln(&format!("Path excludes: {:?}", path_excludes));
    }

    let cmd = frontend_cmd(&language);
    let config = classify::ApgConfig::load(&project_dir);

    // The scanner's stderr (progress + javac diagnostics) goes to the log file
    // so its streaming output never floods the terminal.
    let frontend_err = Stdio::from(log.f.try_clone().expect("clone log file"));
    log.ln("Frontend progress -> apg-frontend.log");

    if language == "cpp" || language == "go" {
        let mut cmd = Command::new(&cmd);
        cmd.arg(project_dir.display().to_string());
        for m in &module_dirs {
            cmd.arg("--module").arg(m);
        }
        cmd.args(&path_excludes)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(frontend_err);
        let mut frontend_output = cmd.spawn().expect("Failed to run frontend");

        process(
            &mut frontend_output,
            &blacklist,
            &path_excludes,
            &language,
            config.as_ref(),
            &mut log,
        );

        log.ln(&format!("[load] waiting on {language} frontend..."));
        if !frontend_output
            .wait()
            .expect("couldn't wait for frontend")
            .success()
        {
            panic!("{language} frontend failed");
        }
        log.ln(&format!("[load] frontend exited"));
    } else {
        let mut args = vec![project_dir.display().to_string()];
        for m in &module_dirs {
            args.push("--module".to_string());
            args.push(m.clone());
        }
        for pat in &path_excludes {
            args.push(pat.clone());
        }
        // `cmd` is the full "java -Xmx5g -cp ... CallGraphBuilder" string;
        // spawn it directly with argv (no `sh -c` wrapper) exactly like the
        // Go/C++ frontends.
        let mut parts = cmd.split_whitespace();
        let prog = parts.next().expect("empty java frontend command");
        let mut frontend_output = Command::new(prog)
            .args(parts)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(frontend_err)
            .spawn()
            .expect("Failed to run Java frontend");

        process(
            &mut frontend_output,
            &blacklist,
            &path_excludes,
            &language,
            config.as_ref(),
            &mut log,
        );

        log.ln("[load] waiting on Java frontend...");
        if !frontend_output
            .wait()
            .expect("couldn't wait for Java frontend")
            .success()
        {
            panic!("java frontend failed");
        }
        log.ln("[load] java frontend exited");
    }
}

/// Consumes the scanner's unified-JSONL stdout, ingests it, and loads
/// `db.lbug` + `graph.jsonl` (SPEC §6).
fn process(
    frontend_output: &mut std::process::Child,
    blacklist: &[String],
    path_excludes: &[String],
    language: &str,
    config: Option<&classify::ApgConfig>,
    log: &mut Log,
) {
    let (mut graph, report) = {
        // Stream the scanner's JSONL straight into the ingestor (which inserts
        // nodes as they arrive and spools edges to disk) rather than buffering
        // every record in memory (SPEC §6).
        let lines = BufReader::new(frontend_output.stdout.as_mut().unwrap()).lines();
        let records = lines.map(|x| {
            let line = x.expect("io error");
            serde_json::from_str::<schema::Record>(&line)
                .unwrap_or_else(|e| panic!("bad json {e}: {line}"))
        });
        ingest::ingest(records, &ingest::IngestOptions {
            blacklist,
            language,
            config,
        })
    };
    log.ln(&format!("Skipped {} blacklisted messages", report.skipped));

    let cleanup_report = cleanup(
        &mut graph,
        &CleanupOptions {
            user_excludes: path_excludes.to_vec(),
            language: language.to_string(),
        },
    );
    log.ln(&format!(
        "cleanup: removed {} nodes, {} contains, {} calls, {} uses, {} unresolved calls, {} unresolved uses, {} span violations",
        cleanup_report.nodes_removed,
        cleanup_report.contains_removed,
        cleanup_report.calls_removed,
        cleanup_report.uses_removed,
        cleanup_report.unresolved_calls_removed,
        cleanup_report.unresolved_uses_removed,
        cleanup_report.span_violations_removed,
    ));

    log.ln(&format!(
        "graph: {} nodes, {} contain edges, {} calls edges, {} uses edges, {} unresolved calls, {} unresolved uses",
        graph.nodes.len(),
        graph.contains.len(),
        graph.calls.len(),
        graph.uses.len(),
        graph.unresolved_calls.len(),
        graph.unresolved_uses.len(),
    ));

    let dir = temp_dir();
    std::fs::create_dir_all(&dir).unwrap();
    log.ln("[load] writing parquet load files...");
    load::build_load_files(&graph, &dir).unwrap();
    log.ln("[load] parquet files written");

    let _ = std::fs::remove_file("db.lbug");
    if std::path::Path::new("db.lbug").exists() {
        panic!("db.lbug still exists (a previous run is still holding it?) — kill any stray java_apg/java processes and retry");
    }
    log.ln("[load] Database::new...");
    let db = Database::new("db.lbug", Default::default()).unwrap();
    log.ln("[load] Database::new done");
    let conn = Connection::new(&db).unwrap();
    log.ln("[load] create_schema...");
    load::create_schema(&conn).unwrap();
    log.ln("[load] schema created");
    log.ln("[load] copy_from...");
    load::copy_from(&conn, &dir).unwrap();
    log.ln("[load] copy_from done");

    log.ln("[load] write_graph_jsonl...");
    load::write_graph_jsonl(&graph, std::path::Path::new("graph.jsonl")).unwrap();
    log.ln("[load] graph.jsonl written");

    log.ln("[load] dropping db...");
    drop(conn);
    drop(db);
    log.ln("[load] db dropped");
    let _ = std::fs::remove_dir_all(&dir);
    log.ln("[load] temp dir removed");
}
