mod classify;
mod cleanup;
mod graph;
mod ingest;
mod load;
mod schema;

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use cleanup::{cleanup, CleanupOptions};
use lbug::{Connection, Database, SystemConfig};

/// The `.opencode/tools/apg_query.ts` tool file that `apg init` installs into a
/// project. Auto-discovered by opencode from `.opencode/tools/*.ts`; shells out
/// to `apg query` (on PATH) from the project root.
const APG_QUERY_TOOL: &str = r#"import { existsSync } from "node:fs"
import path from "node:path"
import { tool } from "@opencode-ai/plugin"

function findApgRoot(context: { directory: string; worktree: string }): string | null {
  const starts = [context.directory, process.cwd(), context.worktree]
  for (const s of starts) {
    if (!s) continue
    let dir = s
    while (true) {
      if (existsSync(path.join(dir, ".apg", "db.lbug"))) return dir
      const parent = path.dirname(dir)
      if (parent === dir) break
      dir = parent
    }
  }
  return null
}

export default tool({
  description:
    "Execute a read-only Cypher query on the project's LadybugDB graph database (.apg/db.lbug). CSV output, header row included. Use for graph traversal: MATCH/RETURN only. No modifications.",
  args: {
    query: tool.schema.string().describe("Cypher query, e.g. MATCH (n:Module) RETURN n.fqn LIMIT 10"),
  },
  async execute(args, context) {
    const root = findApgRoot(context)
    if (!root) {
      return `Error: no .apg/db.lbug found. Run \`apg scan\` in the project root first.`
    }
    const result = await Bun.$`apg query ${args.query}`.cwd(root).quiet().nothrow()
    if (result.exitCode !== 0) {
      return `apg query failed (exit ${result.exitCode}):\n${result.stderr.toString().trim()}`
    }
    return result.stdout.toString().trim()
  },
})
"#;

/// The `.opencode/package.json` written by `apg init` when none exists, so the
/// tool file's `@opencode-ai/plugin` import resolves.
const OPENCODE_PACKAGE_JSON: &str = r#"{
  "dependencies": {
    "@opencode-ai/plugin": "1.18.10"
  }
}
"#;

const DEFAULT_CONFIG_JSON: &str = r#"{
  "default": "src",
  "types": []
}
"#;

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

/// Directory holding the built scanner frontends. Resolution order:
/// 1. `APG_FRONTEND_DIR` env override.
/// 2. Relative to the running executable: `<exe_dir>/frontends` (dev,
///    `target/<profile>/frontends`) or `<exe_dir>/../libexec/frontends`
///    (brew Cellar layout).
/// 3. `None` — fall back to the compile-time baked paths.
fn frontend_dir() -> Option<PathBuf> {
    if let Ok(d) = std::env::var("APG_FRONTEND_DIR") {
        let p = PathBuf::from(d);
        if p.is_dir() {
            return Some(p);
        }
    }
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    [dir.join("frontends"), dir.join("..").join("libexec").join("frontends")]
        .into_iter()
        .find(|c| c.is_dir())
}

fn available_languages() -> Vec<String> {
    if let Some(dir) = frontend_dir() {
        let mut langs = Vec::new();
        if dir.join("cppfrontend").exists() {
            langs.push("cpp".into());
        }
        if dir.join("gofrontend").exists() {
            langs.push("go".into());
        }
        if dir.join("java-classes").is_dir() {
            langs.push("java".into());
        }
        if !langs.is_empty() {
            return langs;
        }
    }
    option_env!("APG_LANGUAGES")
        .unwrap_or("")
        .split(',')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

/// Full command for the scanner frontend of `language`. When a runtime
/// frontend dir is present its artifacts win; otherwise the compile-time baked
/// paths (dev `cargo run`). `None` if no frontend for that language exists.
fn frontend_cmd(language: &str) -> Option<String> {
    if let Some(dir) = frontend_dir() {
        match language {
            "cpp" if dir.join("cppfrontend").exists() => {
                return Some(dir.join("cppfrontend").display().to_string());
            }
            "go" if dir.join("gofrontend").exists() => {
                return Some(dir.join("gofrontend").display().to_string());
            }
            "java" if dir.join("java-classes").is_dir() => {
                let classes = dir.join("java-classes");
                return Some(format!(
                    "java -Xmx5g -cp {} --add-exports jdk.compiler/com.sun.source.tree=ALL-UNNAMED --add-exports jdk.compiler/com.sun.source.util=ALL-UNNAMED --add-exports jdk.compiler/com.sun.tools.javac.tree=ALL-UNNAMED --add-exports jdk.compiler/com.sun.tools.javac.api=ALL-UNNAMED --add-exports jdk.compiler/com.sun.tools.javac.code=ALL-UNNAMED --add-exports jdk.compiler/com.sun.tools.javac.util=ALL-UNNAMED CallGraphBuilder",
                    classes.display()
                ));
            }
            _ => {}
        }
    }
    let baked = match language {
        "cpp" => option_env!("APG_FRONTEND_CPP"),
        "go" => option_env!("APG_FRONTEND_GO"),
        "java" => option_env!("APG_FRONTEND_JAVA"),
        _ => None,
    };
    baked.map(|s| s.to_string())
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
            .is_some_and(|e| exts.contains(&format!(".{}", e.to_str().unwrap_or("")).as_str()))
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

fn temp_dir() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("apg-load-{}-{nanos}", std::process::id()))
}

fn print_help() {
    println!(
        "apg — program graph scanner + LadybugDB query CLI for opencode

USAGE:
  apg init [dir]              Set up .apg/ (db + config) and install the
                              opencode apg_query plugin into .opencode/
  apg scan [dir] [options]    Scan a project; writes .apg/db.lbug and
                              .apg/graph.jsonl
  apg query \"<cypher>\"        Run a read-only Cypher query against
                              .apg/db.lbug (found by walking up from cwd)
  apg --version               Print version
  apg --help                  Show this help

SCAN OPTIONS:
  --language <java|go|cpp>    Scanner language (auto-detected if omitted)
  --exclude-path <glob>       Exclude path patterns (repeatable)
  --module <dir>              Restrict scanning to a module (Go/C++, repeatable)
  <blacklist...>              FQN prefixes to exclude from the graph"
    );
}

fn main() {
    let raw: Vec<String> = std::env::args().collect();
    if raw.len() < 2 {
        print_help();
        std::process::exit(2);
    }
    let status = match raw[1].as_str() {
        "init" => cmd_init(&raw[2..]),
        "query" => cmd_query(&raw[2..]),
        "scan" => cmd_scan(&raw[2..]),
        "--version" | "-V" => {
            println!("apg {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        "--help" | "-h" => {
            print_help();
            Ok(())
        }
        other => {
            eprintln!("apg: unknown subcommand: {other}");
            print_help();
            Err(anyhow::anyhow!("unknown subcommand: {other}"))
        }
    };
    if let Err(e) = status {
        eprintln!("apg: {e}");
        std::process::exit(1);
    }
}

/// `apg init [dir]`: create `.apg/` with a default `config.json`, then install
/// the opencode `apg_query` plugin into `<dir>/.opencode/`.
fn cmd_init(args: &[String]) -> anyhow::Result<()> {
    let dir = if args.is_empty() {
        std::env::current_dir()?
    } else {
        PathBuf::from(&args[0])
    };
    let dir = dir.canonicalize().unwrap_or_else(|_| dir.clone());

    let apg_dir = dir.join(".apg");
    std::fs::create_dir_all(&apg_dir)?;
    let cfg_path = apg_dir.join("config.json");
    if !cfg_path.exists() {
        std::fs::write(&cfg_path, DEFAULT_CONFIG_JSON)?;
    }

    let opencode_dir = dir.join(".opencode");
    let tools_dir = opencode_dir.join("tools");
    std::fs::create_dir_all(&tools_dir)?;

    let pkg_path = opencode_dir.join("package.json");
    if !pkg_path.exists() {
        std::fs::write(&pkg_path, OPENCODE_PACKAGE_JSON)?;
    }
    std::fs::write(tools_dir.join("apg_query.ts"), APG_QUERY_TOOL)?;

    if !opencode_dir
        .join("node_modules")
        .join("@opencode-ai")
        .join("plugin")
        .exists()
    {
        let status = Command::new("npm")
            .arg("install")
            .current_dir(&opencode_dir)
            .status();
        match status {
            Ok(s) if s.success() => {}
            _ => eprintln!(
                "warning: could not npm install in {}; run `npm install` there for the plugin import to resolve.",
                opencode_dir.display()
            ),
        }
    }

    println!(
        "Initialized .apg/ (config.json) and installed apg_query tool in {}",
        opencode_dir.display()
    );
    Ok(())
}

/// `apg query "<cypher>"`: open `.apg/db.lbug` (found by walking up from cwd)
/// read-only and print the result as CSV with a header row.
fn cmd_query(args: &[String]) -> anyhow::Result<()> {
    let query = args.join(" ");
    if query.trim().is_empty() {
        anyhow::bail!("usage: apg query \"<cypher>\"");
    }
    let start = std::env::current_dir()?;
    let apg_dir = find_apg_dir(&start)
        .ok_or_else(|| anyhow::anyhow!("no .apg directory found from {}", start.display()))?;
    let db_path = apg_dir.join("db.lbug");
    if !db_path.exists() {
        anyhow::bail!(
            "{} does not exist — run `apg scan` first",
            db_path.display()
        );
    }

    let query = if query.trim_end().ends_with(';') {
        query
    } else {
        format!("{query};")
    };
    let db = Database::new(&db_path, SystemConfig::default().read_only(true))?;
    let conn = Connection::new(&db)?;
    let result = conn.query(&query)?;
    let names = result.get_column_names();
    let header: Vec<String> = names.iter().map(|n| csv_escape(n)).collect();
    println!("{}", header.join(","));
    for row in result {
        let cells: Vec<String> = row.iter().map(|v| csv_escape(&v.to_string())).collect();
        println!("{}", cells.join(","));
    }
    Ok(())
}

fn csv_escape(field: &str) -> String {
    if field.contains(',') || field.contains('"') || field.contains('\n') {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_string()
    }
}

/// Walks up from `start` looking for a `.apg` directory.
fn find_apg_dir(start: &Path) -> Option<PathBuf> {
    let mut cur = start;
    loop {
        let cand = cur.join(".apg");
        if cand.is_dir() {
            return Some(cand);
        }
        cur = cur.parent()?;
    }
}

/// Finds the project's `.apg` dir (walking up from the scanned dir) or creates
/// one at `<dir>/.apg` if none exists.
fn find_or_create_apg_dir(dir: &Path) -> PathBuf {
    if let Some(apg) = find_apg_dir(dir) {
        return apg;
    }
    let apg = dir.join(".apg");
    std::fs::create_dir_all(&apg).unwrap();
    apg
}

/// `apg scan [dir] [options] [blacklist...]`: run the scanner + ingestor
/// pipeline and write `db.lbug`, `graph.jsonl`, and `apg-frontend.log` into
/// the project's `.apg` directory.
fn cmd_scan(args: &[String]) -> anyhow::Result<()> {
    let mut language: Option<String> = None;
    let mut path_excludes: Vec<String> = Vec::new();
    let mut module_dirs: Vec<String> = Vec::new();
    let mut positional: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--language" | "-l" => {
                i += 1;
                if i < args.len() {
                    language = Some(args[i].clone());
                }
            }
            "--exclude-path" => {
                i += 1;
                if i < args.len() {
                    path_excludes.push(args[i].clone());
                }
            }
            "--module" => {
                i += 1;
                if i < args.len() {
                    module_dirs.push(args[i].clone());
                }
            }
            _ => positional.push(args[i].clone()),
        }
        i += 1;
    }

    let project_dir = if positional.is_empty() {
        std::env::current_dir()?
    } else {
        PathBuf::from(&positional[0])
    };
    let blacklist: Vec<String> = positional.get(1..).unwrap_or(&[]).to_vec();
    let project_dir = project_dir.canonicalize()?;

    // Resolve the .apg output dir, then run the pipeline from inside it so
    // db.lbug / graph.jsonl / apg-frontend.log all land there.
    let apg_dir = find_or_create_apg_dir(&project_dir);
    std::env::set_current_dir(&apg_dir)?;

    let mut log = Log::new();
    log.ln(&format!("Project: {}", project_dir.display()));

    let available = available_languages();
    if available.is_empty() {
        panic!(
            "No scanner frontends found. Install one via brew (e.g. `brew install antz29/apg/apg-go`), set APG_FRONTEND_DIR, or rebuild with the required toolchain."
        );
    }

    let language = language.unwrap_or_else(|| {
        auto_detect_language(&project_dir, &available).unwrap_or_else(|| available[0].clone())
    });

    if !available.iter().any(|l| l == &language) {
        panic!(
            "Language '{language}' is not available. Installed frontends: {}. Install it via brew (e.g. `brew install antz29/apg/apg-{language}`).",
            available.join(", ")
        );
    }
    log.ln(&format!("Language: {language}"));

    if !blacklist.is_empty() {
        log.ln(&format!("Blacklist: {:?}", blacklist));
    }
    if !path_excludes.is_empty() {
        log.ln(&format!("Path excludes: {:?}", path_excludes));
    }

    let cmd = frontend_cmd(&language).unwrap_or_else(|| {
        panic!("frontend for language '{language}' is not installed");
    });
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
        log.ln("[load] frontend exited");
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
    Ok(())
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
        panic!("db.lbug still exists (a previous run is still holding it?) — kill any stray apg/java processes and retry");
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
