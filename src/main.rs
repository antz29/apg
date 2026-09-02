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

/// The opencode tool suite that `apg init` installs into `~/.opencode/`. Each
/// entry is a file under `tools/` (auto-discovered by opencode from
/// `~/.opencode/tools/*.ts`), single-sourced from this repo's own `.opencode`.
/// The files shell out to `apg query` / `apg scan` (on PATH) from the project
/// root; see `.opencode/lib/apg.ts` for the shared plumbing.
const SUITE_TOOLS: &[(&str, &str)] = &[
    (
        "apg_query.ts",
        include_str!("../.opencode/tools/apg_query.ts"),
    ),
    (
        "apg_scan.ts",
        include_str!("../.opencode/tools/apg_scan.ts"),
    ),
    (
        "apg_find_symbol.ts",
        include_str!("../.opencode/tools/apg_find_symbol.ts"),
    ),
    (
        "apg_modules.ts",
        include_str!("../.opencode/tools/apg_modules.ts"),
    ),
    (
        "apg_module_files.ts",
        include_str!("../.opencode/tools/apg_module_files.ts"),
    ),
    (
        "apg_module_structs.ts",
        include_str!("../.opencode/tools/apg_module_structs.ts"),
    ),
    (
        "apg_file_units.ts",
        include_str!("../.opencode/tools/apg_file_units.ts"),
    ),
    (
        "apg_file_path.ts",
        include_str!("../.opencode/tools/apg_file_path.ts"),
    ),
    (
        "apg_methods.ts",
        include_str!("../.opencode/tools/apg_methods.ts"),
    ),
    (
        "apg_struct.ts",
        include_str!("../.opencode/tools/apg_struct.ts"),
    ),
    (
        "apg_callers.ts",
        include_str!("../.opencode/tools/apg_callers.ts"),
    ),
    (
        "apg_callees.ts",
        include_str!("../.opencode/tools/apg_callees.ts"),
    ),
    (
        "apg_uses.ts",
        include_str!("../.opencode/tools/apg_uses.ts"),
    ),
    (
        "apg_unresolved.ts",
        include_str!("../.opencode/tools/apg_unresolved.ts"),
    ),
    (
        "apg_hunk.ts",
        include_str!("../.opencode/tools/apg_hunk.ts"),
    ),
];

/// Shared helper module used by the suite tools (`lib/apg.ts`), installed by
/// `apg init` alongside the tools.
const APG_LIB: &str = include_str!("../.opencode/lib/apg.ts");

/// The `codebase-navigator.md` agent file that `apg init` installs into
/// `~/.opencode/`. Auto-discovered by opencode from `~/.opencode/agents/*.md`;
/// configured to use the apg suite tools and to guide the user through running
/// `apg scan` on the CLI (there is no in-chat scan tool). Single-sourced from
/// the repo's own agent file.
const CODEBASE_NAVIGATOR_AGENT: &str =
    include_str!("../.opencode/agents/codebase-navigator.md");

/// The `package.json` written by `apg init` into `~/.opencode/` when none
/// exists, so the tool files' `@opencode-ai/plugin` import resolves.
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
        if dir.join("rustfrontend").exists() {
            langs.push("rust".into());
        }
        if dir.join("java-classes").is_dir() {
            langs.push("java".into());
        }
        if dir.join("tsfrontend").is_dir() {
            langs.push("ts".into());
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
            "rust" if dir.join("rustfrontend").exists() => {
                return Some(dir.join("rustfrontend").display().to_string());
            }
            "java" if dir.join("java-classes").is_dir() => {
                let classes = dir.join("java-classes");
                return Some(format!(
                    "java -Xmx5g -cp {} --add-exports jdk.compiler/com.sun.source.tree=ALL-UNNAMED --add-exports jdk.compiler/com.sun.source.util=ALL-UNNAMED --add-exports jdk.compiler/com.sun.tools.javac.tree=ALL-UNNAMED --add-exports jdk.compiler/com.sun.tools.javac.api=ALL-UNNAMED --add-exports jdk.compiler/com.sun.tools.javac.code=ALL-UNNAMED --add-exports jdk.compiler/com.sun.tools.javac.util=ALL-UNNAMED CallGraphBuilder",
                    classes.display()
                ));
            }
            "ts" if dir.join("tsfrontend").is_dir() => {
                return Some(format!(
                    "node {}",
                    dir.join("tsfrontend").join("scanner.mjs").display()
                ));
            }
            _ => {}
        }
    }
    let baked = match language {
        "cpp" => option_env!("APG_FRONTEND_CPP"),
        "go" => option_env!("APG_FRONTEND_GO"),
        "rust" => option_env!("APG_FRONTEND_RUST"),
        "java" => option_env!("APG_FRONTEND_JAVA"),
        "ts" => option_env!("APG_FRONTEND_TS"),
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

/// Detects every language present under `dir` (in canonical order), restricted
/// to installed frontends. A multi-language repo returns several entries; a
/// scan then runs each frontend and merges their graphs.
fn auto_detect_languages(dir: &std::path::Path, available: &[String]) -> Vec<String> {
    let candidates: Vec<(&str, &[&str])> = vec![
        ("java", &[".java"] as &[&str]),
        ("go", &[".go"]),
        ("cpp", &[".cpp", ".cc", ".cxx", ".hpp", ".h", ".hh"]),
        ("rust", &[".rs"]),
        ("ts", &[".ts", ".tsx", ".mts", ".cts"]),
    ];
    let mut out = Vec::new();
    for (lang, exts) in &candidates {
        if available.iter().any(|l| l == lang) && has_extension(dir, exts, 5) {
            out.push(lang.to_string());
        }
    }
    out
}

/// Short, language-specific opaque-id prefix (`--id-prefix`) so ids stay
/// globally unique when a scan merges multiple frontend streams (each starts
/// its counter at `n1`). Single-language scans pass no prefix; the frontends
/// default to `n`.
fn id_prefix_for(language: &str) -> &'static str {
    match language {
        "go" => "g",
        "java" => "j",
        "cpp" => "c",
        "rust" => "r",
        "ts" => "t",
        _ => "x",
    }
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
  apg init [dir]              Set up .apg/ (db + config), install/update the
                              opencode apg tool suite + codebase-navigator
                              agent in ~/.opencode/, and remove any legacy
                              project-local .opencode/ install
  apg scan [dir] [options]    Scan a project; writes .apg/db.lbug and
                              .apg/graph.jsonl
  apg query \"<cypher>\"        Run a read-only Cypher query against
                              .apg/db.lbug (found by walking up from cwd)
  apg --version               Print version
  apg --help                  Show this help

SCAN OPTIONS:
  --language <lang>            Scanner language(s): java, go, cpp, rust, ts
                               (comma-separated or repeated; auto-detected for
                               every language present if omitted)
  --exclude-path <glob>       Exclude path patterns (repeatable)
  --module <dir>              Restrict scanning to a module (Go/C++/Rust/TS,
                               repeatable)
  --no-build-scripts          Rust only: skip cargo build scripts and the
                              proc-macro server (hermetic scans)
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

/// The user-level opencode config dir: `~/.opencode`. `apg init` installs the
/// tool suite here (not into the project dir) so it's available to every
/// project's opencode session; tool discovery is project-root based, so the
/// global install works across all projects.
fn user_opencode_dir() -> anyhow::Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|h| h.join(".opencode"))
        .ok_or_else(|| anyhow::anyhow!("could not determine home directory (set $HOME)"))
}

/// Writes `content` to `path` only when the file is missing or its contents
/// differ, so an existing install is updated where required. Returns whether a
/// write happened.
fn write_if_changed(path: &Path, content: &str) -> std::io::Result<bool> {
    match std::fs::read(path) {
        Ok(existing) if existing == content.as_bytes() => Ok(false),
        _ => {
            std::fs::write(path, content)?;
            Ok(true)
        }
    }
}

#[allow(clippy::type_complexity)]
fn dir_entries_only(dir: &Path, names: &[&str]) -> bool {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return true,
    };
    entries.flatten().all(|e| names.contains(&e.file_name().to_string_lossy().as_ref()))
}

/// True when `opencode_dir` is the apg repo's own single-sourced `.opencode/`
/// (the in-tree dir the installed files are compiled from): it carries a
/// `.gitignore` that legacy project installs had no reason to contain, and it
/// is apg-pure — `tools/`, `lib/`, and `agents/` hold only apg-owned files and
/// there is no `skills/` or other user content. A project like `~/platform`
/// that mixes user agents/skills with a stray `.gitignore` is *not* the source
/// dir and its apg files *are* cleaned.
fn is_apg_source_dir(opencode_dir: &Path) -> bool {
    if !opencode_dir.join(".gitignore").is_file() || opencode_dir.join("skills").exists() {
        return false;
    }
    let tool_names: Vec<&str> = SUITE_TOOLS.iter().map(|(n, _)| *n).collect();
    dir_entries_only(&opencode_dir.join("tools"), &tool_names)
        && dir_entries_only(&opencode_dir.join("lib"), &["apg.ts"])
        && dir_entries_only(&opencode_dir.join("agents"), &["codebase-navigator.md"])
}

/// Removes a legacy project-local `.opencode/` apg install written by older
/// `apg init` versions (before the suite moved to `~/.opencode/`). Only
/// apg-owned files are removed: the suite tools (`tools/apg_*.ts`), the shared
/// plumbing (`lib/apg.ts`), the `codebase-navigator` agent, and — only when the
/// `package.json` is byte-identical to the one `apg init` wrote — the
/// apg-generated `package.json`/`package-lock.json`/`bun.lock`/`node_modules`.
/// User-owned agents/tools/skills and modified `package.json` files are left
/// alone (so user content stays when it shares the directory with an old apg
/// install). Returns `(files_removed, dirs_removed)`; `Ok((0, 0))` when there
/// is nothing to clean. The apg repo's own apg-pure `.opencode/` is skipped.
fn remove_legacy_project_install(dir: &Path) -> std::io::Result<(usize, usize)> {
    let opencode_dir = dir.join(".opencode");
    if !opencode_dir.is_dir() || is_apg_source_dir(&opencode_dir) {
        return Ok((0, 0));
    }

    let mut files_removed = 0usize;
    let mut dirs_removed = 0usize;

    let tools_dir = opencode_dir.join("tools");
    for (name, _) in SUITE_TOOLS {
        let p = tools_dir.join(name);
        if p.is_file() && std::fs::remove_file(&p).is_ok() {
            files_removed += 1;
        }
    }
    let lib_dir = opencode_dir.join("lib");
    let lib_apg = lib_dir.join("apg.ts");
    if lib_apg.is_file() && std::fs::remove_file(&lib_apg).is_ok() {
        files_removed += 1;
    }
    let agents_dir = opencode_dir.join("agents");
    let agent_md = agents_dir.join("codebase-navigator.md");
    if agent_md.is_file() && std::fs::remove_file(&agent_md).is_ok() {
        files_removed += 1;
    }

    for d in [&tools_dir, &lib_dir, &agents_dir] {
        if d.is_dir() && std::fs::read_dir(d)?.next().is_none() {
            std::fs::remove_dir(d)?;
            dirs_removed += 1;
        }
    }

    let pkg_path = opencode_dir.join("package.json");
    let apg_owned_pkg = pkg_path
        .is_file()
        && std::fs::read(&pkg_path)
            .map(|b| b == OPENCODE_PACKAGE_JSON.as_bytes())
            .unwrap_or(false);
    if apg_owned_pkg {
        for f in ["package.json", "package-lock.json", "bun.lock"] {
            let p = opencode_dir.join(f);
            if p.is_file() && std::fs::remove_file(&p).is_ok() {
                files_removed += 1;
            }
        }
        let nm = opencode_dir.join("node_modules");
        if nm.is_dir() {
            std::fs::remove_dir_all(&nm)?;
            dirs_removed += 1;
        }
    }

    if apg_owned_pkg
        && std::fs::read_dir(&opencode_dir)
            .map(|mut it| it.next().is_none())
            .unwrap_or(false)
    {
        std::fs::remove_dir(&opencode_dir)?;
        dirs_removed += 1;
    }

    Ok((files_removed, dirs_removed))
}

/// `apg init [dir]`: create `.apg/` with a default `config.json`, install (or
/// update) the opencode `apg_query` plugin + `codebase-navigator` agent into
/// `~/.opencode/`, and remove any legacy project-local `.opencode/` install
/// left by older versions.
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

    let opencode_dir = user_opencode_dir()?;
    let tools_dir = opencode_dir.join("tools");
    std::fs::create_dir_all(&tools_dir)?;
    let agents_dir = opencode_dir.join("agents");
    std::fs::create_dir_all(&agents_dir)?;

    let pkg_path = opencode_dir.join("package.json");
    if !pkg_path.exists() {
        std::fs::write(&pkg_path, OPENCODE_PACKAGE_JSON)?;
    }
    let mut updated = 0usize;
    for (name, content) in SUITE_TOOLS {
        if write_if_changed(&tools_dir.join(name), content)? {
            updated += 1;
        }
    }
    let lib_dir = opencode_dir.join("lib");
    std::fs::create_dir_all(&lib_dir)?;
    if write_if_changed(&lib_dir.join("apg.ts"), APG_LIB)? {
        updated += 1;
    }
    if write_if_changed(
        &agents_dir.join("codebase-navigator.md"),
        CODEBASE_NAVIGATOR_AGENT,
    )? {
        updated += 1;
    }

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

    if updated == 0 {
        println!(
            "Initialized .apg/ (config.json); {} apg tools + codebase-navigator agent already up to date in {}",
            SUITE_TOOLS.len(),
            opencode_dir.display()
        );
    } else {
        println!(
            "Initialized .apg/ (config.json) and installed/updated {} of {} apg tools + codebase-navigator agent in {}",
            updated,
            SUITE_TOOLS.len() + 2,
            opencode_dir.display()
        );
    }

    let (cleaned_files, cleaned_dirs) = remove_legacy_project_install(&dir)?;
    if cleaned_files > 0 || cleaned_dirs > 0 {
        println!(
            "Removed legacy project-local .opencode/ apg install from {} ({} files, {} dirs)",
            dir.join(".opencode").display(),
            cleaned_files,
            cleaned_dirs
        );
    }
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
/// the project's `.apg` directory. A repo may mix languages: auto-detection
/// (or `--language a,b`) runs every frontend present and merges their graphs
/// into one database. Opaque ids are namespaced per language
/// (`--id-prefix`), and a `lang_switch` record before each stream tells the
/// ingestor which language the following records came from (for code_type
/// classification and FQN rendering).
fn cmd_scan(args: &[String]) -> anyhow::Result<()> {
    let mut language_args: Vec<String> = Vec::new();
    let mut path_excludes: Vec<String> = Vec::new();
    let mut module_dirs: Vec<String> = Vec::new();
    let mut no_build_scripts = false;
    let mut positional: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--language" | "-l" => {
                i += 1;
                if i < args.len() {
                    for l in args[i].split(',') {
                        let l = l.trim();
                        if !l.is_empty() {
                            language_args.push(l.to_string());
                        }
                    }
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
            "--no-build-scripts" => {
                no_build_scripts = true;
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

    let languages: Vec<String> = if !language_args.is_empty() {
        for l in &language_args {
            if !available.iter().any(|a| a == l) {
                panic!(
                    "Language '{l}' is not available. Installed frontends: {}. Install it via brew (e.g. `brew install antz29/apg/apg-{l}`).",
                    available.join(", ")
                );
            }
        }
        language_args
    } else {
        let detected = auto_detect_languages(&project_dir, &available);
        if detected.is_empty() {
            available.clone()
        } else {
            detected
        }
    };
    log.ln(&format!("Languages: {}", languages.join(", ")));

    if !blacklist.is_empty() {
        log.ln(&format!("Blacklist: {:?}", blacklist));
    }
    if !path_excludes.is_empty() {
        log.ln(&format!("Path excludes: {:?}", path_excludes));
    }

    let config = classify::ApgConfig::load(&project_dir);

    // The scanners' stderr (progress + compiler diagnostics) goes to the log
    // file so their streaming output never floods the terminal.
    let frontend_err = |log: &mut Log| Stdio::from(log.f.try_clone().expect("clone log file"));
    log.ln("Frontend progress -> apg-frontend.log");

    // Drain each frontend's stdout to a temp file (spooled to disk, never
    // buffered in memory), then ingest the merged streams. Running them
    // sequentially avoids pipe-backpressure deadlock and matches the old
    // single-frontend behavior.
    let tmp = temp_dir();
    std::fs::create_dir_all(&tmp).unwrap();
    let multi = languages.len() > 1;
    let mut spools: Vec<(String, PathBuf)> = Vec::new();
    for lang in &languages {
        let cmd = frontend_cmd(lang).unwrap_or_else(|| {
            panic!("frontend for language '{lang}' is not installed");
        });
        let spool = tmp.join(format!("{lang}.jsonl"));
        let spool_file = std::fs::File::create(&spool).unwrap();
        // `cmd` is a full command line (e.g. "node /path/scanner.mjs" or the
        // java wrapper); split it into argv so every frontend spawns the same
        // way.
        let mut parts = cmd.split_whitespace();
        let prog = parts.next().expect("empty frontend command");
        let mut child = Command::new(prog);
        child
            .args(parts)
            .arg(project_dir.display().to_string());
        for m in &module_dirs {
            child.arg("--module").arg(m);
        }
        if *lang == "rust" && no_build_scripts {
            child.arg("--no-build-scripts");
        }
        if multi {
            child.arg("--id-prefix").arg(id_prefix_for(lang));
        }
        child
            .args(&path_excludes)
            .stdin(Stdio::null())
            .stdout(Stdio::from(spool_file.try_clone().unwrap()))
            .stderr(frontend_err(&mut log));
        log.ln(&format!("[scan] running {lang} frontend..."));
        let mut frontend_output = child.spawn().expect("Failed to run frontend");
        if !frontend_output
            .wait()
            .expect("couldn't wait for frontend")
            .success()
        {
            panic!("{lang} frontend failed");
        }
        log.ln(&format!("[scan] {lang} frontend exited"));
        spools.push((lang.clone(), spool));
    }

    // Merge the streams into one record iterator, with a `lang_switch` record
    // before each language's records so the ingestor classifies and renders
    // each under the right language.
    let iterators: Vec<Box<dyn Iterator<Item = schema::Record>>> = spools
        .into_iter()
        .map(|(lang, spool)| {
            let lines =
                BufReader::new(std::fs::File::open(&spool).unwrap()).lines();
            let records = lines.map(|x| {
                let line = x.expect("io error");
                serde_json::from_str::<schema::Record>(&line)
                    .unwrap_or_else(|e| panic!("bad json {e}: {line}"))
            });
            Box::new(
                std::iter::once(schema::Record::LangSwitch { language: lang }).chain(records),
            ) as Box<dyn Iterator<Item = schema::Record>>
        })
        .collect();
    let records = iterators.into_iter().flatten();

    // Cleanup span validation is per-language: keep the single-language value,
    // and disable it (by joining) for mixed scans where the check cannot be
    // attributed per node.
    let cleanup_language = if languages.len() == 1 {
        languages[0].clone()
    } else {
        languages.join(",")
    };

    run_pipeline(
        records,
        &blacklist,
        &path_excludes,
        &cleanup_language,
        config.as_ref(),
        &mut log,
    );
    let _ = std::fs::remove_dir_all(&tmp);
    log.ln("[scan] spool temp dir removed");
    Ok(())
}

/// Consumes the merged scanner JSONL stream, ingests it, and loads `db.lbug` +
/// `graph.jsonl` (SPEC §6).
fn run_pipeline(
    records: impl IntoIterator<Item = schema::Record>,
    blacklist: &[String],
    path_excludes: &[String],
    language: &str,
    config: Option<&classify::ApgConfig>,
    log: &mut Log,
) {
    let (mut graph, report) = {
        // Stream the scanner JSONL straight into the ingestor (which inserts
        // nodes as they arrive and spools edges to disk) rather than buffering
        // every record in memory (SPEC §6).
        ingest::ingest(records, &ingest::IngestOptions {
            blacklist,
            language,
            config,
        })
    };
    log.ln(&format!("Skipped {} blacklisted messages", report.skipped));
    if report.shadowed_modules > 0 {
        log.ln(&format!(
            "{} module(s) shadowed by a type of the same name (package/type collision; type wins)",
            report.shadowed_modules
        ));
    }
    if report.shadowed_functions > 0 {
        log.ln(&format!(
            "{} function(s) shadowed by a struct of the same FQN (struct wins)",
            report.shadowed_functions
        ));
    }

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
