mod artifacts;
mod classify;
mod cleanup;
mod git;
mod graph;
mod ingest;
mod layers;
mod load;
mod node_cmd;
mod plan_cmd;
mod project_cmd;
mod review_cmd;
mod schema;
mod session;
mod specs;
#[cfg(test)]
mod testutil;
mod version_gate;

use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use cleanup::{CleanupOptions, cleanup};
use lbug::{Connection, Database, SystemConfig};

/// The opencode tool suite that `apg init` installs into `~/.opencode/`. Each
/// entry is a file under `tools/` (auto-discovered by opencode from
/// `~/.opencode/tools/*.ts`), single-sourced from this repo's `opencode-suite/`.
/// The files shell out to `apg query` / `apg scan` (on PATH) from the project
/// root; see `opencode-suite/lib/apg.ts` for the shared plumbing.
const SUITE_TOOLS: &[(&str, &str)] = &[
    (
        "apg_query.ts",
        include_str!("../opencode-suite/tools/apg_query.ts"),
    ),
    (
        "apg_scan.ts",
        include_str!("../opencode-suite/tools/apg_scan.ts"),
    ),
    (
        "apg_find_symbol.ts",
        include_str!("../opencode-suite/tools/apg_find_symbol.ts"),
    ),
    (
        "apg_modules.ts",
        include_str!("../opencode-suite/tools/apg_modules.ts"),
    ),
    (
        "apg_module_files.ts",
        include_str!("../opencode-suite/tools/apg_module_files.ts"),
    ),
    (
        "apg_module_structs.ts",
        include_str!("../opencode-suite/tools/apg_module_structs.ts"),
    ),
    (
        "apg_file_units.ts",
        include_str!("../opencode-suite/tools/apg_file_units.ts"),
    ),
    (
        "apg_file_path.ts",
        include_str!("../opencode-suite/tools/apg_file_path.ts"),
    ),
    (
        "apg_methods.ts",
        include_str!("../opencode-suite/tools/apg_methods.ts"),
    ),
    (
        "apg_struct.ts",
        include_str!("../opencode-suite/tools/apg_struct.ts"),
    ),
    (
        "apg_callers.ts",
        include_str!("../opencode-suite/tools/apg_callers.ts"),
    ),
    (
        "apg_callees.ts",
        include_str!("../opencode-suite/tools/apg_callees.ts"),
    ),
    (
        "apg_uses.ts",
        include_str!("../opencode-suite/tools/apg_uses.ts"),
    ),
    (
        "apg_unresolved.ts",
        include_str!("../opencode-suite/tools/apg_unresolved.ts"),
    ),
    (
        "apg_hunk.ts",
        include_str!("../opencode-suite/tools/apg_hunk.ts"),
    ),
    // Durable node-file mutation surface (apg-projects SPEC §2.2/§4).
    (
        "apg_node.ts",
        include_str!("../opencode-suite/tools/apg_node.ts"),
    ),
    (
        "apg_edge.ts",
        include_str!("../opencode-suite/tools/apg_edge.ts"),
    ),
    // Project lifecycle (start / verify / merge).
    (
        "apg_project.ts",
        include_str!("../opencode-suite/tools/apg_project.ts"),
    ),
    // Plan/review suite.
    (
        "apg_review.ts",
        include_str!("../opencode-suite/tools/apg_review.ts"),
    ),
    (
        "apg_review_add.ts",
        include_str!("../opencode-suite/tools/apg_review_add.ts"),
    ),
    (
        "apg_review_action.ts",
        include_str!("../opencode-suite/tools/apg_review_action.ts"),
    ),
    (
        "apg_review_resolve.ts",
        include_str!("../opencode-suite/tools/apg_review_resolve.ts"),
    ),
    (
        "apg_review_reject.ts",
        include_str!("../opencode-suite/tools/apg_review_reject.ts"),
    ),
    (
        "apg_plan.ts",
        include_str!("../opencode-suite/tools/apg_plan.ts"),
    ),
    (
        "apg_plan_phases.ts",
        include_str!("../opencode-suite/tools/apg_plan_phases.ts"),
    ),
    (
        "apg_plan_tasks.ts",
        include_str!("../opencode-suite/tools/apg_plan_tasks.ts"),
    ),
    (
        "apg_plan_complete.ts",
        include_str!("../opencode-suite/tools/apg_plan_complete.ts"),
    ),
    (
        "apg_plan_render.ts",
        include_str!("../opencode-suite/tools/apg_plan_render.ts"),
    ),
    (
        "apg_plan_add.ts",
        include_str!("../opencode-suite/tools/apg_plan_add.ts"),
    ),
    (
        "apg_plan_done.ts",
        include_str!("../opencode-suite/tools/apg_plan_done.ts"),
    ),
    (
        "apg_plan_undone.ts",
        include_str!("../opencode-suite/tools/apg_plan_undone.ts"),
    ),
    (
        "apg_plan_note.ts",
        include_str!("../opencode-suite/tools/apg_plan_note.ts"),
    ),
    (
        "apg_plan_verify.ts",
        include_str!("../opencode-suite/tools/apg_plan_verify.ts"),
    ),
];

/// Shared helper module used by the suite tools (`lib/apg.ts`), installed by
/// `apg init` alongside the tools.
const APG_LIB: &str = include_str!("../opencode-suite/lib/apg.ts");

/// The layout-upgrade guide that `apg init` installs into
/// `~/.opencode/lib/apg-upgrade.md` next to the shared lib: version-field
/// meaning, mismatch detection, upgrade steps (re-run init → re-scan),
/// common-issue fixes. The R10 version gate's block text points at it.
///
/// Maintained inline here; if the suite tree (`opencode-suite/lib/`) ever
/// gains an `apg-upgrade.md`, this const should flip to an `include_str!` of
/// it like `APG_LIB` above.
const APG_UPGRADE_DOC: &str = r#"# apg layout upgrades — the `apg/config.json` version field

This guide is installed by `apg init` at `~/.opencode/lib/apg-upgrade.md`;
the version gate's block text points here.

## What the `version` field is

`apg/config.json` (the committed layout config at the repo root) carries a
**binary-managed `version` field** — for example `"version": "0.10.4"`. It
records which apg binary last initialized or upgraded the layout. It is not a
user setting: `apg init` owns it. Your `default` / `types` (code_type rules)
are yours; the `version` field is the binary's — never hand-edit it.

## What checks it

The layout-touching operations — `apg scan` and `apg project start` —
**block, they never warn**, unless the layout's declared version shares the
running binary's **major.minor** (patch versions never matter):

| apg/config.json `version` | apg 0.10.4 verdict |
|---|---|
| same major.minor (`0.10.0` … `0.10.99`) | proceed (patch diff is fine) |
| missing `version` field (pre-versioning layout) | **block** |
| older major.minor (`0.9.x`, `0.8.x`, …) | **block** — upgrade the layout |
| newer major.minor (`0.11.x`, `1.x`, …) | **block** — upgrade the binary |

`apg init` is the upgrade act: it re-runs idempotently, writes the current
version, and scaffolds the layout. `apg init` is also the layout entry point —
a repo never created by `apg init` has no versioned layout at all.

## Upgrade steps

1. Re-run **`apg init`** from the repo root. It is idempotent: writes the
   current binary version into `apg/config.json` (your code_type rules are
   untouched), scaffolds `apg/.worktrees/` + the `.gitignore` entries
   (`apg/.trans/`, `apg/.worktrees/`), and installs/updates the opencode apg
   suite (tools, agents, this guide).
2. Re-run the blocked command: `apg scan`, or `apg project start <name>`.
3. **Layout newer than the binary** (the block text says so): upgrade apg
   first — `brew upgrade apg` (or the matching frontend formulae), the
   `install.sh` installer, or a newer release — then `apg init`, then re-run
   the blocked command.

## Common issues

- **"declares no layout version"** — the layout predates layout versioning,
  or was created without `apg init` (an old `apg scan` created only
  `apg/.trans/`). Fix: `apg init` writes the field.
- **"not a valid version"** — the field was hand-edited into something
  unparseable. Fix: `apg init` rewrites it.
- **"config.json does not exist"** — no versioned layout here yet. Fix:
  `apg init` (the layout entry point), then re-run.
- **Worktree scaffolding missing** — `apg init` scaffolds `apg/.worktrees/`
  and its `.gitignore` entry; `apg project start` also self-heals both when
  absent, so this resolves itself on the next start.
- **Code_type rules look reformatted** — the JSON file is rewritten when the
  version changes; the rules' *content* is preserved (only whitespace/field
  order may normalize). Never re-add the version by hand afterwards — re-run
  `apg init`.
"#;

/// The `codebase-navigator.md` agent file that `apg init` installs into
/// `~/.opencode/`. Auto-discovered by opencode from `~/.opencode/agents/*.md`;
/// configured to use the apg suite tools and to guide the user through running
/// `apg scan` on the CLI (there is no in-chat scan tool). Single-sourced from
/// the repo's own agent file.
const CODEBASE_NAVIGATOR_AGENT: &str =
    include_str!("../opencode-suite/agents/codebase-navigator.md");

/// The six distributed agents that `apg init` installs into `~/.opencode/agents/`
/// (SPEC R13/R15): the navigator plus the five spec/plan/review/builder agents,
/// single-sourced from the repo's `opencode-suite/agents/`.
const AGENTS: &[(&str, &str)] = &[
    ("codebase-navigator.md", CODEBASE_NAVIGATOR_AGENT),
    (
        "spec-writer.md",
        include_str!("../opencode-suite/agents/spec-writer.md"),
    ),
    (
        "plan-writer.md",
        include_str!("../opencode-suite/agents/plan-writer.md"),
    ),
    (
        "spec-review.md",
        include_str!("../opencode-suite/agents/spec-review.md"),
    ),
    (
        "plan-review.md",
        include_str!("../opencode-suite/agents/plan-review.md"),
    ),
    (
        "agent-builder.md",
        include_str!("../opencode-suite/agents/agent-builder.md"),
    ),
];

/// The six distributed agent filenames — the apg-owned names in
/// `~/.opencode/agents/`. `apg init` prunes any of these that a newer release
/// dropped from the suite (a user's own same-named agent is the accepted edge).
const KNOWN_AGENT_FILES: &[&str] = &[
    "codebase-navigator.md",
    "spec-writer.md",
    "plan-writer.md",
    "spec-review.md",
    "plan-review.md",
    "agent-builder.md",
];

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
pub(crate) struct Log {
    f: std::fs::File,
}

impl Log {
    pub(crate) fn new() -> Log {
        Log {
            f: std::fs::File::create("apg-frontend.log")
                .expect("failed to create apg-frontend.log"),
        }
    }

    fn ln(&mut self, msg: &str) {
        eprintln!("{msg}");
        let _ = writeln!(self.f, "{msg}");
    }

    /// Appends a spooled file's contents to the log only (no terminal echo),
    /// used to fold a frontend's captured stderr into `apg-frontend.log`.
    fn append_file(&mut self, path: &Path) {
        if let Ok(content) = std::fs::read_to_string(path) {
            let _ = write!(self.f, "{content}");
        }
    }
}

/// Last `n` non-empty-ish lines of a file, oldest first (a short tail for
/// reporting why a frontend failed).
fn tail_of(path: &Path, n: usize) -> Vec<String> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let lines: Vec<&str> = content.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].iter().map(|s| s.to_string()).collect()
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
    [
        dir.join("frontends"),
        dir.join("..").join("libexec").join("frontends"),
    ]
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
        if dir.join("csharpfrontend").exists() || dir.join("csharpfrontend.exe").exists() {
            langs.push("csharp".into());
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
            "csharp"
                if dir.join("csharpfrontend").exists()
                    || dir.join("csharpfrontend.exe").exists() =>
            {
                let bin = if dir.join("csharpfrontend").exists() {
                    dir.join("csharpfrontend")
                } else {
                    dir.join("csharpfrontend.exe")
                };
                return Some(bin.display().to_string());
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
        "csharp" => option_env!("APG_FRONTEND_CSHARP"),
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
        ("csharp", &[".cs", ".csx"]),
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
        "csharp" => "cs",
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

/// The `apg --help` text. Split from [`print_help`] so the strict add|update|rm
/// surface and the unchanged `apg review` line can be asserted directly by the
/// Phase-7 help/dispatch acceptance test (no stdout capture).
fn help_text() -> String {
    "apg — program graph scanner + LadybugDB query CLI for opencode

USAGE:
  apg init [dir]              Set up apg/ (config.json carrying the binary
                              version + .trans/ + .worktrees/), scaffold the
                              repo .gitignore for the apg layout entries,
                              install/update the opencode apg tool suite + six
                              distributed agents + the upgrade guide in
                              ~/.opencode/, and warn loudly about project
                              .opencode/ files that duplicate the installed
                              suite (never deletes)
  apg scan [dir] [options]    Scan a project; writes apg/.trans/db.lbug and
                              apg/.trans/graph.jsonl
  apg query [--json] \"<cypher>\"  Run a read-only Cypher query against
                               apg/.trans/db.lbug (found by walking up from
                               cwd); CSV by default, --json for JSON rows
  apg plan <sub> …            The phased execution plan (transient, branch-local):
                              add/update/rm/done/undone/note/complete/render/verify
                              (add <project> creates the plan, then authors
                              phases, tasks, and planned Implementation nodes —
                              module/file/struct/function marked planned at the
                              FQN where the code lands; update edits in place;
                              rm refuses while dependents exist unless --force;
                              verify is the pre-merge coherence gate)
  apg project <sub> …         Project contexts (worktrees, git2-operated):
                              start <name> — worktree + branch + branch DB off
                              the default branch (apg/.worktrees/<name>);
                              merge <name> — verify gate → merge → main rebuild;
                              delete <name> — abandon a project: remove its
                              worktree + delete its branch (commits discarded;
                              refuses unsafe states, never the default branch)
  apg review <sub> …          Writer↔reviewer feedback cycle:
                              add/action/resolve/reject/list
  apg node <sub> …            Durable node-file model mutations:
                              add/update/rm (type-as-argument, writes apg/layers;
                              the name is identity and is never updatable)
  apg edge <sub> …            Durable node-file model edge mutations:
                              add/update/rm (kind/from/to; update is
                              properties-only)
  apg session <sub> …         Session-scoped single-writer coordinator
                              (apg/.trans/session.sock):
                              start — own db.lbug exclusively, perform routed
                              node/edge mutations and serve routed reads in
                              receive order (write-through, no buffered flush);
                              end — signal the live session to release the
                              DB/socket and exit
  apg --version               Print version
  apg --help                  Show this help

SCAN OPTIONS:
  --language <lang>            Scanner language(s): java, go, cpp, rust, ts,
                               csharp (comma-separated or repeated; auto-detected
                               for every language present if omitted)
  --exclude-path <glob>       Exclude path patterns (repeatable)
  --module <dir>              Restrict scanning to a module (Go/C++/Rust/TS/C#,
                               repeatable)
  --no-build-scripts          Rust only: skip cargo build scripts and the
                              proc-macro server (hermetic scans)
  <blacklist...>              FQN prefixes to exclude from the graph"
        .to_string()
}

fn print_help() {
    println!("{}", help_text());
}

/// `apg session <start|end>` (phase-03): `start` launches the session-scoped
/// single-writer coordinator (bind socket, own `db.lbug`, serve routed
/// mutations/reads); `end` signals the running session — the server-side
/// shutdown releases the DB/socket and `serve` exits. Every mutation's
/// projection delta was already applied write-through, so `end` performs no
/// flush.
fn session_cmd(args: &[String]) -> anyhow::Result<()> {
    let apg_root = session::require_apg_root()?;
    match args.first().map(|s| s.as_str()) {
        Some("start") => session::Coordinator::start(&apg_root),
        Some("end") => session::Coordinator::signal_end(&apg_root),
        other => anyhow::bail!(
            "usage: apg session <start|end> (got `{}`)",
            other.unwrap_or("<none>")
        ),
    }
}

/// The `apg` entry point: dispatches the CLI subcommands (`init`, `query`,
/// `scan`, `plan`, `review`, `project`, `node`, `edge`, `session`), prints help
/// for `--help`/no args, and turns a returned error into a non-zero exit. The
/// binary embeds the apg opencode suite — the tool set (`SUITE_TOOLS`) and the
/// six distributed agents (`AGENTS`) delivered by `apg init` — whose prompts
/// carry the coordinator-mediated feedback cycle: the owning writer returns an
/// ACTIONED/WONT-FIX claim and the coordinator performs the shallow
/// claim-vs-change consistency check and then actions the item.
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
        "plan" => plan_cmd::cmd_plan(&raw[2..]),
        "review" => review_cmd::cmd_review(&raw[2..]),
        "project" => project_cmd::cmd_project(&raw[2..]),
        "node" => node_cmd::cmd_node(&raw[2..]),
        "edge" => node_cmd::cmd_edge(&raw[2..]),
        "session" => session_cmd(&raw[2..]),
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

/// Files in a project's local `.opencode/` that duplicate the user-level
/// install (`~/.opencode/`): the same relative path exists in both. A project
/// `.opencode/` should hold only project-specific agents (agent-builder
/// generated); a copy of a suite tool or core agent there shadows the installed
/// version, so `apg init` warns loudly about it — it never deletes anything.
fn duplicate_install_files(project_opencode: &Path, user_opencode: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, project: &Path, user: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                // Dependency trees are not part of the installed suite.
                if p.file_name().is_some_and(|n| n == "node_modules") {
                    continue;
                }
                walk(&p, project, user, out);
            } else if let Ok(rel) = p.strip_prefix(project)
                && user.join(rel).is_file()
                && !is_dep_manifest(p.file_name())
            {
                out.push(p);
            }
        }
    }
    let mut out = Vec::new();
    if project_opencode.is_dir() {
        walk(project_opencode, project_opencode, user_opencode, &mut out);
    }
    out
}

/// The per-project npm manifests + git hygiene files that `apg init` also
/// writes into `~/.opencode/` (`package.json`, lockfiles, `.gitignore`) are not
/// suite files; a project's own copies are not shadows.
fn is_dep_manifest(name: Option<&std::ffi::OsStr>) -> bool {
    matches!(
        name.and_then(|n| n.to_str()),
        Some("package.json" | "package-lock.json" | "bun.lock" | ".gitignore")
    )
}

/// Prune stale apg-managed files from the user-level install (`~/.opencode/`):
/// `tools/apg_*.ts` (the `apg_` prefix is the apg namespace) and the known
/// distributed agent names that a newer release removed from the suite. Anything
/// not owned by apg (a user's own tools/agents) is preserved. `apg init` already
/// overwrites edited suite files via `write_if_changed`, so removing the same
/// owned set is consistent. Returns the number of files pruned.
fn prune_stale_suite(opencode_dir: &Path) -> std::io::Result<usize> {
    let current_tools: std::collections::HashSet<&str> =
        SUITE_TOOLS.iter().map(|(n, _)| *n).collect();
    let current_agents: std::collections::HashSet<&str> = AGENTS.iter().map(|(n, _)| *n).collect();
    let mut pruned = 0;

    let tools_dir = opencode_dir.join("tools");
    if let Ok(entries) = std::fs::read_dir(&tools_dir) {
        for e in entries.flatten() {
            let p = e.path();
            let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if name.starts_with("apg_") && name.ends_with(".ts") && !current_tools.contains(name) {
                std::fs::remove_file(&p)?;
                pruned += 1;
            }
        }
    }

    let agents_dir = opencode_dir.join("agents");
    if let Ok(entries) = std::fs::read_dir(&agents_dir) {
        for e in entries.flatten() {
            let p = e.path();
            let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if KNOWN_AGENT_FILES.contains(&name) && !current_agents.contains(name) {
                std::fs::remove_file(&p)?;
                pruned += 1;
            }
        }
    }

    Ok(pruned)
}

/// Installs (or updates) the apg suite into `opencode_dir`: every `SUITE_TOOLS`
/// file, the shared lib, the upgrade guide, and the six distributed agents —
/// each written only when missing or changed — then prunes stale apg-owned
/// files. Returns `(files written/updated, files pruned)`. `cmd_init` calls it
/// against `~/.opencode`; tests call it against a temp dir. The package.json
/// scaffold is only written when absent (never clobbered). The embedded
/// agent/tool prose it delivers carries the coordinator-mediated feedback
/// cycle: the owning writer returns an ACTIONED/WONT-FIX claim and the
/// coordinator performs the shallow claim-vs-change check and then actions the
/// item (`apg_review_action`).
fn install_suite(opencode_dir: &Path) -> anyhow::Result<(usize, usize)> {
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
    // The upgrade guide the R10 gate's block text points at (task-4).
    // Best-effort with a warning: an unwritable ~/.opencode must not fail
    // init (mirrors the npm-install failure handling below).
    let upgrade_doc_path = lib_dir.join("apg-upgrade.md");
    match write_if_changed(&upgrade_doc_path, APG_UPGRADE_DOC) {
        Ok(true) => updated += 1,
        Ok(false) => {}
        Err(e) => eprintln!(
            "warning: could not install the upgrade guide into {} ({e}); the version-gate block text points at ~/.opencode/lib/apg-upgrade.md — re-run `apg init` once the location is writable",
            upgrade_doc_path.display()
        ),
    }
    for (name, content) in AGENTS {
        if write_if_changed(&agents_dir.join(name), content)? {
            updated += 1;
        }
    }
    let pruned = prune_stale_suite(opencode_dir)?;
    Ok((updated, pruned))
}

/// `apg init [dir]`: create the committed `apg/` layout (config.json carrying
/// the binary-managed layout `version` + `.trans/` + the project-worktrees
/// dir), install (or update) the opencode apg tool suite + the six
/// distributed agents + the upgrade guide into `~/.opencode/`, scaffold the
/// repo `.gitignore` for the apg layout entries (`apg/.trans/`,
/// `apg/.worktrees/`), and warn loudly about any project-local `.opencode/`
/// files that duplicate the installed suite (never deletes anything).
/// Project-specific implementer/reviewer agents are installed into the
/// project `.opencode/` by the agent-builder, not by init. Init is the
/// layout's versioning/upgrade act (R9/R10): it re-runs idempotently and
/// writes the binary version into `apg/config.json` (user code_type rules
/// untouched) — `apg scan` and `apg project start` refuse to touch a layout
/// whose version is missing or does not share the binary's major.minor.
fn cmd_init(args: &[String]) -> anyhow::Result<()> {
    let dir = if args.is_empty() {
        std::env::current_dir()?
    } else {
        PathBuf::from(&args[0])
    };
    let dir = dir.canonicalize().unwrap_or_else(|_| dir.clone());

    let apg_dir = dir.join(specs::LAYOUT);
    std::fs::create_dir_all(apg_dir.join(specs::TRANS))?;
    // The project-worktrees dir (gitignored; `project start` nests each
    // project's worktree under it). Scaffolded even before git exists — the
    // dir itself is what libgit2's worktree add needs as a parent.
    std::fs::create_dir_all(apg_dir.join(git::WORKTREES))?;
    let cfg_path = apg_dir.join("config.json");
    if !cfg_path.exists() {
        std::fs::write(&cfg_path, DEFAULT_CONFIG_JSON)?;
    }
    // The binary-managed version field (R9): init is the layout's upgrade
    // act — write-through on every init, idempotent when already current.
    version_gate::ensure_config_version(&apg_dir, env!("CARGO_PKG_VERSION"))?;

    let opencode_dir = user_opencode_dir()?;
    let (updated, pruned) = install_suite(&opencode_dir)?;

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
            "Initialized apg/ (config.json v{} + .trans/ + .worktrees/); {} suite files + {} agents already up to date in {}",
            env!("CARGO_PKG_VERSION"),
            SUITE_TOOLS.len() + 2,
            AGENTS.len(),
            opencode_dir.display()
        );
    } else {
        println!(
            "Initialized apg/ (config.json v{} + .trans/ + .worktrees/) and installed/updated {} of {} suite files + {} agents in {}",
            env!("CARGO_PKG_VERSION"),
            updated,
            SUITE_TOOLS.len() + 2,
            AGENTS.len(),
            opencode_dir.display()
        );
    }
    if pruned > 0 {
        println!(
            "pruned {} stale apg suite file(s) from {} (removed from the suite in a newer release)",
            pruned,
            opencode_dir.display()
        );
    }

    scaffold_gitignore(&dir)?;

    let project_opencode = dir.join(".opencode");
    let dupes = duplicate_install_files(&project_opencode, &opencode_dir);
    if !dupes.is_empty() {
        eprintln!(
            "!! WARNING: {} file(s) in {} duplicate the installed apg suite in {}:",
            dupes.len(),
            project_opencode.display(),
            opencode_dir.display()
        );
        for p in &dupes {
            eprintln!("     {}", p.strip_prefix(&dir).unwrap_or(p).display());
        }
        eprintln!(
            "   The project .opencode should hold only project-specific agents (installed by \
             agent-builder); these shadow the installed versions. Remove them if unintended."
        );
    }
    Ok(())
}

/// The apg-owned entries `apg init` scaffolds into the repo `.gitignore`:
/// the gitignored transient `apg/.trans/` (db, export, plans, renders, logs)
/// and the project worktrees dir `apg/.worktrees/` — one entry covers every
/// `<project>` nested under it (R9). Durable `apg/` data (config, specs,
/// notes) stays committed above both.
const GITIGNORE_APG_ENTRIES: &[&str] = &["apg/.trans/", "apg/.worktrees/"];

/// Ensures the repo `.gitignore` carries the apg layout entries (each added
/// if missing; other lines untouched), so the durable `apg/` data is
/// committed and only transient state (db, export, plans, renders, logs) and
/// project worktrees are ignored. `apg init` scaffolds both; `apg project
/// start` calls the same scaffolder to self-heal a repo whose entries were
/// dropped (it commits the change itself). Returns whether the file was
/// modified.
pub(crate) fn scaffold_gitignore(dir: &Path) -> anyhow::Result<bool> {
    let p = dir.join(".gitignore");
    let content = std::fs::read_to_string(&p).unwrap_or_default();
    let lines: Vec<&str> = content.lines().collect();
    let mut missing: Vec<&str> = Vec::new();
    for entry in GITIGNORE_APG_ENTRIES {
        // Both spellings count as present (git treats `dir/` and `dir`
        // equivalently for a directory entry).
        let bare = entry.trim_end_matches('/');
        let present = lines.iter().any(|l| l.trim().trim_end_matches('/') == bare);
        if !present {
            missing.push(entry);
        }
    }
    if missing.is_empty() {
        return Ok(false);
    }
    let mut out = content.clone();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(
        "# apg layout entries (transient/worktree dirs; committed apg/ data lives above them)\n",
    );
    for entry in &missing {
        out.push_str(entry);
        out.push('\n');
    }
    std::fs::write(&p, out)?;
    Ok(true)
}

/// `apg query "<cypher>"`: open `apg/.trans/db.lbug` (found by walking up from
/// cwd) read-only and print the result as CSV with a header row.
fn cmd_query(args: &[String]) -> anyhow::Result<()> {
    let json = args.first().is_some_and(|a| a == "--json");
    let query = if json {
        args[1..].join(" ")
    } else {
        args.join(" ")
    };
    if query.trim().is_empty() {
        anyhow::bail!("usage: apg query [--json] \"<cypher>\"");
    }
    let start = std::env::current_dir()?;
    let apg_root = find_apg_root(&start)
        .ok_or_else(|| anyhow::anyhow!("no apg/ directory found from {}", start.display()))?;

    let query = if query.trim_end().ends_with(';') {
        query
    } else {
        format!("{query};")
    };

    // Read routing (phase-03): while a session owns db.lbug, route the read
    // through it so a separate reader sees the current state with no lock error
    // and without waiting for the session to end. With no live session the DB
    // file is not held and a normal (read-only) direct open serves the read —
    // that is the read-your-writes guarantee.
    if session::live_session(&apg_root) {
        let output = session::forward_query(&apg_root, &query, json)?;
        println!("{output}");
        return Ok(());
    }

    let db_path = apg_root.join(specs::TRANS).join("db.lbug");
    if !db_path.exists() {
        anyhow::bail!(
            "{} does not exist — run `apg scan` first",
            db_path.display()
        );
    }
    let db = Database::new(&db_path, SystemConfig::default().read_only(true))?;
    println!("{}", render_query(&db, &query, json)?);
    Ok(())
}

/// Render a query result exactly as `apg query` prints it: JSON rows for
/// `--json`, otherwise CSV with a header row (no trailing newline). Shared by
/// the direct path and the session coordinator's routed-read branch, so both
/// produce byte-identical output.
pub(crate) fn render_query(db: &Database, query: &str, json: bool) -> anyhow::Result<String> {
    let conn = Connection::new(db)?;
    let result = conn.query(query)?;
    if json {
        return Ok(emit_json_rows(result));
    }
    let names = result.get_column_names();
    let mut out = names
        .iter()
        .map(|n| csv_escape(n))
        .collect::<Vec<_>>()
        .join(",");
    for row in result {
        out.push('\n');
        out.push_str(
            &row.iter()
                .map(|v| csv_escape(&v.to_string()))
                .collect::<Vec<_>>()
                .join(","),
        );
    }
    Ok(out)
}

/// Renders a query result as a JSON array of objects, one per row, keyed by
/// column name with string-typed values (matching CSV's cell semantics).
fn emit_json_rows(result: lbug::QueryResult<'_>) -> String {
    let names = result.get_column_names();
    let rows: Vec<serde_json::Value> = result
        .map(|row| {
            let mut obj = serde_json::Map::new();
            for (i, name) in names.iter().enumerate() {
                let v = row.get(i).map(|v| v.to_string()).unwrap_or_default();
                obj.insert(name.clone(), serde_json::Value::String(v));
            }
            serde_json::Value::Object(obj)
        })
        .collect();
    serde_json::to_string_pretty(&serde_json::Value::Array(rows)).unwrap()
}

fn csv_escape(field: &str) -> String {
    if field.contains(',') || field.contains('"') || field.contains('\n') {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_string()
    }
}

/// Walks up from `start` looking for the committed `apg/` layout root.
fn find_apg_root(start: &Path) -> Option<PathBuf> {
    specs::find_apg_root(start)
}

/// Finds the project's `apg/` layout root (walking up from the scanned dir) or
/// creates one at `<dir>/apg` (with `.trans/`) if none exists.
fn find_or_create_apg_root(dir: &Path) -> PathBuf {
    specs::find_or_create_apg_root(dir)
}

/// Build the scanner JSONL record stream from the frontend spools: a
/// `lang_switch` record before each language's records plus the leading
/// `scan_meta` control record. Borrows the spool paths (reopening each file)
/// so it can be run twice — once for the pre-ingest that computes
/// `ingest_tree`'s scanned code-FQN universe, then again for the real
/// pipeline.
fn scanner_records<'a>(
    spools: &'a [(String, PathBuf)],
    git_state: &'a git::GitState,
) -> impl Iterator<Item = schema::Record> + 'a {
    let iterators = spools.iter().map(|(lang, spool)| {
        let lines = BufReader::new(std::fs::File::open(spool).unwrap()).lines();
        let records = lines.map(|x| {
            let line = x.expect("io error");
            serde_json::from_str::<schema::Record>(&line)
                .unwrap_or_else(|e| panic!("bad json {e}: {line}"))
        });
        Box::new(
            std::iter::once(schema::Record::LangSwitch {
                language: lang.clone(),
            })
            .chain(records),
        ) as Box<dyn Iterator<Item = schema::Record>>
    });
    let records = iterators.into_iter().flatten();
    std::iter::once(schema::Record::ScanMeta {
        git_sha: git_state.sha.clone(),
        git_clean: git_state.sha.as_ref().map(|_| git_state.clean),
        scanned_at: git::now_iso8601(),
    })
    .chain(records)
}

/// `apg scan [dir] [options] [blacklist...]`: run the scanner + ingestor
/// pipeline and write `db.lbug`, `graph.jsonl`, and `apg-frontend.log` into
/// the project's `.apg` directory. A repo may mix languages: auto-detection
/// (or `--language a,b`) runs every frontend present and merges their graphs
/// into one database. Opaque ids are namespaced per language
/// (`--id-prefix`), and a `lang_switch` record before each stream tells the
/// ingestor which language the following records came from (for code_type
/// classification and FQN rendering). A `scan_meta` control record leads the
/// whole stream with the git state the scan ran under (recorded as the DB's
/// `Scan` node and graph.jsonl line 1).
pub(crate) fn cmd_scan(args: &[String]) -> anyhow::Result<()> {
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

    // The git state this scan runs under (repo HEAD sha + tree cleanliness),
    // recorded as the scan_meta control record and the DB's Scan node so later
    // spec/plan/review mutations can refuse to run against a stale DB.
    let git_state = git::git_state(&project_dir);

    // R10 version gate: `apg scan` is a layout-touching op, so the repo must
    // carry a versioned layout — `apg init` is the layout entry/upgrade act
    // that writes the binary version into apg/config.json. A missing version
    // (pre-versioning layout) or a major/minor mismatch in either direction
    // blocks with upgrade guidance; the gate never warns. The gate runs
    // BEFORE find_or_create below so a refusal never leaves a stray,
    // unversioned layout behind.
    let gate_root =
        specs::find_apg_root(&project_dir).unwrap_or_else(|| project_dir.join(specs::LAYOUT));
    version_gate::require_layout_version(&gate_root, "re-run `apg scan`")?;

    // Resolve the committed `apg/` layout root, then run the pipeline from
    // inside its gitignored `.trans/` so db.lbug / graph.jsonl /
    // apg-frontend.log all land there (the committed `apg/` data — config,
    // specs, notes — stays in the root).
    let apg_root = find_or_create_apg_root(&project_dir);
    // Lifecycle exclusivity (phase-03): a scan replaces `db.lbug` (it unlinks
    // and rebuilds it), which would silently diverge the graph a live session
    // holds open. Refuse BEFORE any of that work — the session must end first.
    if session::live_session(&apg_root) {
        anyhow::bail!(
            "refused: a live `apg session` owns {} — end it first (`apg session end` inside the project worktree) before scanning",
            apg_root.join(specs::TRANS).join("db.lbug").display()
        );
    }
    let trans_dir = apg_root.join(specs::TRANS);
    std::fs::create_dir_all(&trans_dir)?;
    std::env::set_current_dir(&trans_dir)?;

    let mut log = Log::new();
    log.ln(&format!("Project: {}", project_dir.display()));
    // Staleness of the pre-scan DB vs the tree (STALE/FRESH/N-A).
    log.ln(&git::staleness_line(&apg_root, &git_state));

    let available = available_languages();
    if available.is_empty() {
        panic!(
            "No scanner frontends found. Install one via brew (e.g. `brew install antz29/apg/apg-go`), the curl installer (e.g. `install.sh go`), set APG_FRONTEND_DIR, or rebuild with the required toolchain."
        );
    }

    let languages: Vec<String> = if !language_args.is_empty() {
        for l in &language_args {
            if !available.iter().any(|a| a == l) {
                panic!(
                    "Language '{l}' is not available. Installed frontends: {}. Install it via brew (e.g. `brew install antz29/apg/apg-{l}`) or the curl installer (`install.sh {l}`).",
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

    // Each frontend's stderr (progress + compiler diagnostics) is spooled to a
    // per-language temp file, then folded into the log file. On a non-zero
    // exit the tail is also reported to the terminal (SPEC 0.9.1 R1).
    log.ln("Frontend progress -> apg-frontend.log");

    // Drain each frontend's stdout to a temp file (spooled to disk, never
    // buffered in memory), then ingest the merged streams. Running them
    // sequentially avoids pipe-backpressure deadlock and matches the old
    // single-frontend behavior. A failing frontend is reported and skipped, not
    // fatal: the remaining languages still produce a graph, and a non-zero exit
    // is aggregated at the end of the run.
    let tmp = temp_dir();
    std::fs::create_dir_all(&tmp).unwrap();
    let multi = languages.len() > 1;
    let mut spools: Vec<(String, PathBuf)> = Vec::new();
    let mut failed: Vec<String> = Vec::new();
    for lang in &languages {
        let cmd = frontend_cmd(lang).unwrap_or_else(|| {
            panic!("frontend for language '{lang}' is not installed");
        });
        let spool = tmp.join(format!("{lang}.jsonl"));
        let spool_file = std::fs::File::create(&spool).unwrap();
        let stderr_spool = tmp.join(format!("{lang}.stderr"));
        let stderr_file = std::fs::File::create(&stderr_spool).unwrap();
        // `cmd` is a full command line (e.g. "node /path/scanner.mjs" or the
        // java wrapper); split it into argv so every frontend spawns the same
        // way.
        let mut parts = cmd.split_whitespace();
        let prog = parts.next().expect("empty frontend command");
        let mut child = Command::new(prog);
        child.args(parts).arg(project_dir.display().to_string());
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
            .stderr(Stdio::from(stderr_file.try_clone().unwrap()));
        log.ln(&format!("[scan] running {lang} frontend..."));
        let mut frontend_output = child.spawn().expect("Failed to run frontend");
        let ok = frontend_output
            .wait()
            .expect("couldn't wait for frontend")
            .success();
        log.append_file(&stderr_spool);
        if !ok {
            log.ln(&format!(
                "[scan] {lang} frontend failed; skipping this language"
            ));
            for line in tail_of(&stderr_spool, 10) {
                log.ln(&format!("  [{lang}] {line}"));
            }
            failed.push(lang.clone());
            continue;
        }
        log.ln(&format!("[scan] {lang} frontend exited"));
        spools.push((lang.clone(), spool));
    }

    // Merge the streams into one record iterator, with a `lang_switch` record
    // before each language's records so the ingestor classifies and renders
    // each under the right language. A `scan_meta` control record (the git
    // state this scan ran under) leads the whole stream; the ingestor turns it
    // into the DB's `Scan` node and the export puts it on graph.jsonl line 1.
    //
    // The scanner stream is built by a helper (borrowing the spool paths) so
    // it can be read twice: once for a pre-ingest that computes the scanned
    // code-FQN universe (the honest renderer reuse for `ingest_tree`'s
    // `implemented-by` validation), and once for the real pipeline.
    let records = scanner_records(&spools, &git_state);

    // Cleanup span validation is per-language: keep the single-language value,
    // and disable it (by joining) for mixed scans where the check cannot be
    // attributed per node.
    let cleanup_language = if languages.len() == 1 {
        languages[0].clone()
    } else {
        languages.join(",")
    };

    // Pre-ingest the scanner stream to compute the scanned code-FQN universe
    // `ingest_tree` validates `implemented-by` refs against (real → real).
    let scanned_code: BTreeSet<String> = {
        let (pre, _) = ingest::ingest(
            scanner_records(&spools, &git_state),
            &ingest::IngestOptions {
                blacklist: &blacklist,
                language: &cleanup_language,
                config: config.as_ref(),
            },
        );
        pre.nodes
            .iter()
            .filter(|(_, n)| {
                matches!(
                    n.kind,
                    graph::NodeKind::Module
                        | graph::NodeKind::Struct
                        | graph::NodeKind::Function
                        | graph::NodeKind::File
                ) && n.status.is_none()
            })
            .map(|(f, _)| f.clone())
            .collect()
    };

    // Read the transient legs (`.trans/plans/*.jsonl` — the per-branch plan
    // store — plus the five `.trans/<tier>/*.jsonl` feedback mirrors, SPEC
    // §5: feedback sits in the tier dir of its attached node, both halves of
    // the relationship in `.trans`) into both the planned-FQN universe
    // `ingest_tree` uses (planned → pending) and the records the pipeline
    // chains after code.
    let transient_files = specs::plan_files(&apg_root)
        .into_iter()
        .chain(specs::trans_mirror_files(&apg_root))
        .collect::<Vec<_>>();
    let mut transient_records: Vec<schema::Record> = Vec::new();
    let mut planned: BTreeSet<String> = BTreeSet::new();
    for f in &transient_files {
        for r in specs::read_jsonl(f).unwrap_or_else(|e| panic!("{e:#}")) {
            if let schema::Record::PlannedNode { fqn, .. } = &r {
                planned.insert(fqn.clone());
            }
            transient_records.push(r);
        }
    }

    // Ingest the durable `apg/layers/` tree into the new-model records,
    // validating pairing / code-refs / constraints (R14/R16) against the
    // scanned graph and the planned-node universe.
    let layers_records = layers::ingest_tree(&apg_root, &scanned_code, &planned)?;

    if !layers_records.is_empty() || !transient_records.is_empty() {
        log.ln(&format!(
            "Layer tree + transient inputs: {} layer-node records, {} transient records",
            layers_records.len(),
            transient_records.len(),
        ));
    }

    let records = records.chain(layers_records).chain(transient_records);

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
    if !failed.is_empty() {
        anyhow::bail!(
            "{} frontend(s) failed to scan (partial graph written): {}",
            failed.len(),
            failed.join(", ")
        );
    }
    Ok(())
}

/// Consumes the merged scanner JSONL stream, ingests it, and loads `db.lbug` +
/// `graph.jsonl` (SPEC §6).
pub(crate) fn run_pipeline(
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
        ingest::ingest(
            records,
            &ingest::IngestOptions {
                blacklist,
                language,
                config,
            },
        )
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

    // Defense in depth (phase-03 lifecycle exclusivity): never unlink a DB a
    // live session holds. `cmd_scan` refuses earlier; this guard catches a
    // session that started mid-scan before the projected DB is replaced.
    if let Ok(cwd) = std::env::current_dir()
        && let Some(apg_root) = cwd.parent()
        && session::live_session(apg_root)
    {
        panic!(
            "refused: a live apg session owns this db.lbug — run `apg session end` before scanning"
        );
    }
    let _ = std::fs::remove_file("db.lbug");
    if std::path::Path::new("db.lbug").exists() {
        panic!(
            "db.lbug still exists (a previous run is still holding it?) — kill any stray apg/java processes and retry"
        );
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
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_json_emits_rows() {
        // `emit_json_rows` renders a query result as a JSON array of objects,
        // one per row, keyed by column name with string-typed values. The DB
        // is built through the real load pipeline (schema + copy_from).
        use crate::graph::{Graph, Location, Node, NodeKind};

        let dir = std::env::temp_dir().join(format!("apg-qj-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(specs::TRANS)).unwrap();

        let mut graph = Graph::default();
        graph.nodes.insert(
            "github.com/x/y".to_string(),
            Node {
                kind: NodeKind::Module,
                ..Node::default()
            },
        );
        graph.nodes.insert(
            "github.com/x/y.Store".to_string(),
            Node {
                kind: NodeKind::Struct,
                location: Some(Location {
                    path: "/abs/store.go".into(),
                    start: 0,
                    end: 40,
                    start_line: 1,
                    end_line: 40,
                }),
                code_type: "src".to_string(),
                ..Node::default()
            },
        );

        let ldir = dir.join(specs::TRANS).join("load");
        std::fs::create_dir_all(&ldir).unwrap();
        load::build_load_files(&graph, &ldir).unwrap();
        let db = Database::new(dir.join(specs::TRANS).join("db.lbug"), Default::default()).unwrap();
        let conn = Connection::new(&db).unwrap();
        load::create_schema(&conn).unwrap();
        load::copy_from(&conn, &ldir).unwrap();

        let result = conn.query("MATCH (n:Struct) RETURN n.fqn").unwrap();
        let out = emit_json_rows(result);
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        let arr = parsed.as_array().expect("array of rows");
        assert_eq!(arr.len(), 1, "one struct row: {out}");
        assert_eq!(arr[0]["n.fqn"], "github.com/x/y.Store");
        assert!(arr[0].get("n.fqn").is_some(), "keyed by column name");

        drop(conn);
        drop(db);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn duplicate_install_files_detects_overlap() {
        let proj = std::env::temp_dir().join(format!("apg-proj-oc-{}", std::process::id()));
        let user = std::env::temp_dir().join(format!("apg-user-oc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&proj);
        let _ = std::fs::remove_dir_all(&user);
        std::fs::create_dir_all(proj.join("agents")).unwrap();
        std::fs::create_dir_all(proj.join("tools")).unwrap();
        std::fs::create_dir_all(user.join("tools")).unwrap();
        std::fs::create_dir_all(user.join("agents")).unwrap();
        // Duplicate: same relative path exists in both.
        std::fs::write(proj.join("tools").join("apg_query.ts"), "x").unwrap();
        std::fs::write(user.join("tools").join("apg_query.ts"), "x").unwrap();
        // Project-only (a generated agent): not a duplicate.
        std::fs::write(proj.join("agents").join("implementer.md"), "x").unwrap();
        // User-only (a core agent): not a duplicate.
        std::fs::write(user.join("agents").join("spec-writer.md"), "x").unwrap();
        // node_modules trees + dep manifests exist in both but are not shadows.
        let shared_dep = proj
            .join("node_modules")
            .join("@opencode-ai")
            .join("plugin")
            .join("dist")
            .join("index.js");
        std::fs::create_dir_all(shared_dep.parent().unwrap()).unwrap();
        std::fs::write(&shared_dep, "x").unwrap();
        std::fs::create_dir_all(
            user.join("node_modules")
                .join("@opencode-ai")
                .join("plugin")
                .join("dist"),
        )
        .unwrap();
        std::fs::write(
            user.join("node_modules")
                .join("@opencode-ai")
                .join("plugin")
                .join("dist")
                .join("index.js"),
            "x",
        )
        .unwrap();
        std::fs::write(proj.join("package.json"), "{}").unwrap();
        std::fs::write(user.join("package.json"), "{}").unwrap();
        std::fs::write(proj.join("package-lock.json"), "{}").unwrap();
        std::fs::write(user.join("package-lock.json"), "{}").unwrap();
        // A project .opencode/.gitignore is git hygiene, not a suite shadow.
        std::fs::write(proj.join(".gitignore"), "node_modules\n").unwrap();
        std::fs::write(user.join(".gitignore"), "node_modules\n").unwrap();
        let dupes = duplicate_install_files(&proj, &user);
        assert_eq!(dupes.len(), 1);
        assert!(dupes.contains(&proj.join("tools").join("apg_query.ts")));
        let _ = std::fs::remove_dir_all(&proj);
        let _ = std::fs::remove_dir_all(&user);
    }

    #[test]
    fn duplicate_install_files_absent_when_no_project_opencode() {
        let proj = std::env::temp_dir().join(format!("apg-no-oc-{}", std::process::id()));
        let user = std::env::temp_dir().join(format!("apg-no-oc-user-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&proj);
        let _ = std::fs::remove_dir_all(&user);
        std::fs::create_dir_all(&user).unwrap();
        let dupes = duplicate_install_files(&proj, &user);
        assert!(dupes.is_empty());
        let _ = std::fs::remove_dir_all(&proj);
        let _ = std::fs::remove_dir_all(&user);
    }

    #[test]
    fn prune_stale_suite_removes_apg_files_preserves_others() {
        let dir = std::env::temp_dir().join(format!("apg-prune-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("tools")).unwrap();
        std::fs::create_dir_all(dir.join("agents")).unwrap();
        // Stale apg tool (no longer in the suite): pruned.
        std::fs::write(dir.join("tools").join("apg_oldtool.ts"), "x").unwrap();
        // Current apg tool: kept.
        std::fs::write(dir.join("tools").join("apg_query.ts"), "x").unwrap();
        // Non-apg tool: preserved.
        std::fs::write(dir.join("tools").join("my_custom_tool.ts"), "x").unwrap();
        // A current distributed agent: kept.
        std::fs::write(dir.join("agents").join("codebase-navigator.md"), "x").unwrap();
        // A user's own agent: preserved.
        std::fs::write(dir.join("agents").join("my-reviewer.md"), "x").unwrap();

        let pruned = prune_stale_suite(&dir).unwrap();
        assert_eq!(pruned, 1);
        assert!(!dir.join("tools").join("apg_oldtool.ts").exists());
        assert!(dir.join("tools").join("apg_query.ts").exists());
        assert!(dir.join("tools").join("my_custom_tool.ts").exists());
        assert!(dir.join("agents").join("codebase-navigator.md").exists());
        assert!(dir.join("agents").join("my-reviewer.md").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The suite install derives from SUITE_TOOLS: the new project/layers
    /// tools (apg_node/apg_edge/apg_project/apg_plan_verify) embed + install,
    /// and the retired spec/invariant tools are gone from both the embed list
    /// and the installed set (a stale file in the target is pruned).
    #[test]
    fn suite_installs_node_edge_project_tools_and_retires_spec_invariant() {
        let dir = std::env::temp_dir().join(format!("apg-suite-install-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // A stale pre-rewrite tool already present in the target: pruned by
        // the install (the old `apg spec`/`apg invariant` surfaces are gone).
        std::fs::create_dir_all(dir.join("tools")).unwrap();
        std::fs::write(dir.join("tools").join("apg_spec.ts"), "stale").unwrap();

        let (updated, pruned) = install_suite(&dir).unwrap();
        assert!(updated > 0, "a fresh install writes files");
        assert_eq!(pruned, 1, "the retired apg_spec.ts is pruned");

        for name in [
            "apg_node.ts",
            "apg_edge.ts",
            "apg_project.ts",
            "apg_plan_verify.ts",
        ] {
            let p = dir.join("tools").join(name);
            assert!(p.exists(), "{name} must install");
            let content = std::fs::read_to_string(&p).unwrap();
            assert!(
                content.contains("@opencode-ai/plugin"),
                "{name} must be a real tool file"
            );
        }
        for name in [
            "apg_spec.ts",
            "apg_spec_add.ts",
            "apg_spec_requirements.ts",
            "apg_invariants.ts",
            "apg_invariant_add.ts",
            "apg_plan_apply.ts",
        ] {
            assert!(
                !dir.join("tools").join(name).exists(),
                "{name} must not install"
            );
        }
        // The embed list itself: new tools present, old tools gone.
        let names: Vec<&str> = SUITE_TOOLS.iter().map(|(n, _)| *n).collect();
        for name in [
            "apg_node.ts",
            "apg_edge.ts",
            "apg_project.ts",
            "apg_plan_verify.ts",
        ] {
            assert!(names.contains(&name), "SUITE_TOOLS embeds {name}");
        }
        assert!(
            names.iter().all(|n| !n.starts_with("apg_spec")),
            "no apg_spec tools remain in SUITE_TOOLS"
        );
        assert!(
            !names.contains(&"apg_invariants.ts") && !names.contains(&"apg_invariant_add.ts"),
            "no apg_invariant tools remain in SUITE_TOOLS"
        );
        assert!(
            !names.contains(&"apg_plan_apply.ts"),
            "apg_plan_apply was renamed verify"
        );

        // R23 strict surface: the retired plan tools are gone from both the
        // embed list and the installed set.
        for name in ["apg_plan_init.ts", "apg_plan_link.ts"] {
            assert!(
                !names.contains(&name),
                "retired {name} must not be embedded"
            );
            assert!(
                !dir.join("tools").join(name).exists(),
                "retired {name} must not install"
            );
        }

        // The re-scoped/new wrappers dispatch the add|update|rm surface:
        // apg_plan_add also creates the plan (no `kind`); node/edge expose
        // update/rm.
        for name in ["apg_plan_add.ts", "apg_node.ts", "apg_edge.ts"] {
            let content = std::fs::read_to_string(dir.join("tools").join(name)).unwrap();
            assert!(
                content.contains("\"update\"") && content.contains("\"rm\""),
                "{name} must expose the update/rm actions"
            );
        }
        let plan_add = std::fs::read_to_string(dir.join("tools").join("apg_plan_add.ts")).unwrap();
        assert!(
            plan_add.contains("\"plan\"") && plan_add.contains("--force"),
            "apg_plan_add.ts is the plan add/update/rm wrapper"
        );

        // Ripple consumers: no shipped tool, agent prompt, or AGENTS.md text
        // names a retired verb.
        let agents_md = include_str!("../AGENTS.md");
        for banned in ["plan init", "plan link", "apg_plan_init", "apg_plan_link"] {
            for (name, content) in SUITE_TOOLS {
                assert!(
                    !content.contains(banned),
                    "tool {name} names the retired verb `{banned}`"
                );
            }
            for (name, content) in AGENTS {
                assert!(
                    !content.contains(banned),
                    "agent prompt {name} names the retired verb `{banned}`"
                );
            }
            assert!(
                !agents_md.contains(banned),
                "AGENTS.md names the retired verb `{banned}`"
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The read-guard prose holds for exactly the five prompts this change
    /// touches: the four distributed agents rewritten by read-guard-prose
    /// (spec-review/spec-writer/plan-writer/plan-review) plus agent-builder.md,
    /// whose common-shape and step-6 verify text make every generated agent
    /// inherit the rule. `codebase-navigator.md` is deliberately excluded (see
    /// below).
    #[test]
    fn installed_agent_prompts_state_file_access_read_guard() {
        const RULE: &str = "graph state is reached only through the apg tools";
        const NEVER_READ: &str = "never read directly";
        const BLANKET: &str = "You may read any file";

        for name in [
            "spec-review.md",
            "spec-writer.md",
            "plan-writer.md",
            "plan-review.md",
            "agent-builder.md",
        ] {
            let content = AGENTS
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, c)| *c)
                .unwrap_or_else(|| panic!("{name} is in the embedded AGENTS set"));
            assert!(
                content.contains(RULE),
                "{name} must state that graph state is reached only through the apg tools"
            );
            assert!(
                content.contains(NEVER_READ),
                "{name} must state that node/transient files are never read directly"
            );
            assert!(
                !content.contains(BLANKET),
                "{name} must not carry the blanket `{BLANKET}` claim"
            );
        }

        // `codebase-navigator.md` is excluded from the assertion above: it is a
        // sixth entry in the embedded AGENTS set that neither phase edits (it
        // already reaches graph state through the tools and never reads raw
        // files, per codebase-navigator.md's database section), so a literal
        // whole-set guard would fail on an unchanged file. Only the five
        // in-scope prompts are asserted.

        // The inheritance half inspects the agent-builder.md TEMPLATE text
        // (there is no generated-agent artifact to read): it must require the
        // positive rule and fail a generated agent whose body claims broader
        // read access than its grant.
        let builder = AGENTS
            .iter()
            .find(|(n, _)| *n == "agent-builder.md")
            .map(|(_, c)| *c)
            .unwrap();
        assert!(
            builder.contains(RULE) && builder.contains(NEVER_READ),
            "agent-builder.md must require the positive read-guard rule"
        );
        assert!(
            builder.contains("claims broader read access than its grant"),
            "agent-builder.md's verify checklist must fail a broader-than-grant body"
        );
    }

    /// The coordinator-mediated feedback cycle is embedded in the shipped
    /// prose: the navigator holds the `apg_review_action` grant and carries the
    /// dispatch protocol (dispatch one open item to its owning writer → receive
    /// the single ACTIONED/WONT-FIX claim → shallow claim-vs-change check →
    /// action or re-dispatch); the two reviewer prompts and the
    /// `apg_review_action` tool point the action step at the coordinator, never
    /// the writer.
    #[test]
    fn coordinator_agent_carries_review_action_and_dispatch_prose() {
        fn agent(name: &str) -> &'static str {
            AGENTS
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, c)| *c)
                .unwrap_or_else(|| panic!("{name} is in the embedded AGENTS set"))
        }

        // The navigator is the coordinator: it gains the action grant in its
        // permission frontmatter and the dispatch-protocol prose.
        let navigator = agent("codebase-navigator.md");
        assert!(
            navigator.contains("apg_review_action: allow"),
            "the navigator prompt must hold the apg_review_action grant"
        );
        for needle in [
            "coordinator",
            "ACTIONED/WONT-FIX claim",
            "shallow",
            "re-dispatch",
        ] {
            assert!(
                navigator.contains(needle),
                "the navigator prompt must carry the dispatch protocol ({needle})"
            );
        }
        assert!(
            !navigator.contains("and actions Feedback"),
            "the navigator must no longer claim the implementer actions Feedback"
        );

        // The reviewer prompts point the action step at the coordinator: the
        // writer returns a claim, the coordinator actions it.
        for name in ["spec-review.md", "plan-review.md"] {
            let prompt = agent(name);
            assert!(
                prompt.contains("coordinator: apg_review_action"),
                "{name} must show the coordinator running apg_review_action"
            );
            assert!(
                prompt.contains("ACTIONED/WONT-FIX"),
                "{name} must state that the writer returns a claim"
            );
            assert!(
                !prompt.contains("writer:   apg_review_action")
                    && !prompt.contains("writer: apg_review_action"),
                "{name} must not show the writer running apg_review_action"
            );
        }

        // The action tool's own description names the coordinator as the actor.
        let tool = SUITE_TOOLS
            .iter()
            .find(|(n, _)| *n == "apg_review_action.ts")
            .map(|(_, c)| *c)
            .expect("apg_review_action.ts is in SUITE_TOOLS");
        assert!(
            tool.contains("coordinator"),
            "the apg_review_action tool must name the coordinator"
        );
        assert!(
            !tool.contains("Only the writer side does this"),
            "the apg_review_action tool must not say only the writer actions"
        );
    }

    #[test]
    fn scaffold_gitignore_adds_layout_entries_once() {
        let d = std::env::temp_dir().join(format!("apg-gitignore-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join(".gitignore"), "/target\n").unwrap();
        assert!(scaffold_gitignore(&d).unwrap(), "first scaffold writes");
        let once = std::fs::read_to_string(d.join(".gitignore")).unwrap();
        assert!(once.contains("apg/.trans/"));
        assert!(once.contains("apg/.worktrees/"));
        assert!(
            once.starts_with("/target\n"),
            "other lines untouched: {once}"
        );
        assert!(
            !scaffold_gitignore(&d).unwrap(),
            "idempotent scaffold writes nothing"
        );
        let twice = std::fs::read_to_string(d.join(".gitignore")).unwrap();
        assert_eq!(once, twice);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn scaffold_gitignore_accepts_existing_entries_in_either_spelling() {
        let d = std::env::temp_dir().join(format!("apg-gitignore-spell-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        // No trailing slashes: both entries are present for git purposes.
        std::fs::write(d.join(".gitignore"), "apg/.trans\napg/.worktrees\n").unwrap();
        assert!(!scaffold_gitignore(&d).unwrap());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The upgrade guide (task-4) must cover the version field's meaning,
    /// the mismatch detection, and the upgrade steps — the R10 block text
    /// points users at it.
    #[test]
    fn upgrade_doc_covers_version_field_gate_and_fix_steps() {
        for needle in [
            "version",
            "apg/config.json",
            "apg scan",
            "apg project start",
            "apg init",
            "major.minor",
            "apg-upgrade.md",
        ] {
            assert!(
                APG_UPGRADE_DOC.contains(needle),
                "doc must mention {needle}"
            );
        }
    }

    /// The version this release gate guards. Bump this literal in lockstep with
    /// the `[package] version` line on every release: the assertions below fail
    /// on any drift (manifest/lockfile/compiled constant ahead of or behind the
    /// advertised release), so a bump commit cannot silently skip it.
    const RELEASE_VERSION: &str = "0.12.3";

    /// The `version = "..."` declared directly under a Cargo.toml `[package]`
    /// header.
    fn cargo_manifest_version(manifest: &str) -> Option<&str> {
        let in_package = manifest
            .lines()
            .position(|l| l.trim() == "[package]")
            .map(|i| i + 1)?;
        manifest.lines().skip(in_package).find_map(|l| {
            l.trim()
                .strip_prefix("version = ")
                .map(|v| v.trim_matches('"'))
        })
    }

    /// The `version = "..."` of the named `[[package]]` entry in a Cargo.lock.
    fn cargo_lock_package_version<'a>(lock: &'a str, name: &str) -> Option<&'a str> {
        let lines: Vec<&str> = lock.lines().collect();
        let start = lines.iter().position(|l| l.trim() == "[[package]]")?;
        let mut pkg = String::new();
        for l in lines.iter().skip(start) {
            let t = l.trim();
            if t == "[[package]]" {
                pkg.clear();
                continue;
            }
            if let Some(n) = t.strip_prefix("name = ") {
                pkg = n.trim_matches('"').to_string();
            } else if let Some(v) = t.strip_prefix("version = ")
                && pkg == name
            {
                return Some(v.trim_matches('"'));
            }
        }
        None
    }

    #[test]
    fn cargo_manifest_and_lockfile_declare_release_version() {
        let root = env!("CARGO_MANIFEST_DIR");
        let manifest = std::fs::read_to_string(format!("{root}/Cargo.toml")).unwrap();
        let lock = std::fs::read_to_string(format!("{root}/Cargo.lock")).unwrap();
        // The version the crate was actually compiled at must be the release
        // this test guards (env! comes from the same Cargo.toml, so this also
        // catches a test that drifted ahead of the bump).
        assert_eq!(env!("CARGO_PKG_VERSION"), RELEASE_VERSION);
        assert_eq!(cargo_manifest_version(&manifest), Some(RELEASE_VERSION));
        assert_eq!(
            cargo_lock_package_version(&lock, "apg"),
            Some(RELEASE_VERSION)
        );
    }

    #[test]
    fn readme_documents_release_version() {
        let readme =
            std::fs::read_to_string(format!("{}/README.md", env!("CARGO_MANIFEST_DIR"))).unwrap();
        // The README pins the 0.12.x line, not an exact patch, so patch releases
        // don't require a README edit.
        assert!(readme.contains("apg 0.12.x"), "README --version examples");
        assert!(readme.contains("0.12.x"), "README tagged-release prose");
        assert!(
            readme.contains("--version 0.12.x"),
            "README Linux installer pin option"
        );
        // No stale release records: the previous versions must be fully replaced.
        assert!(!readme.contains("0.10"), "README must not reference 0.10");
        assert!(!readme.contains("0.11"), "README must not reference 0.11");
    }

    /// `&[&str]` → the `Vec<String>` argv shape `cmd_node`/`cmd_edge`/`cmd_plan`
    /// take (the top-level dispatch slice `main` would hand them).
    fn argv(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    /// Runs `f` with the process cwd temporarily set to `dir` — the top-level
    /// `cmd_*` dispatch functions resolve `apg/` by walking up from cwd.
    /// Serialized behind the shared cwd lock so it never interleaves with a
    /// concurrent `scan_checkout`.
    fn with_cwd<T>(dir: &Path, f: impl FnOnce() -> T) -> T {
        let _guard = testutil::CWD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let old = std::env::current_dir().unwrap();
        std::env::set_current_dir(dir).unwrap();
        let out = f();
        std::env::set_current_dir(old).unwrap();
        out
    }

    /// A real project context for the strict-surface acceptance sweep: a git
    /// repo whose worktree `foo` on branch `foo` carries a real branch DB
    /// (scanned from a committed hermetic payload) and a fresh scan_meta — so
    /// the top-level `cmd_node`/`cmd_edge`/`cmd_plan` dispatch runs exactly as
    /// it does inside a project. Returns `(wt_apg_root, repo, wt_root)`.
    fn strict_surface_fixture(tag: &str) -> (PathBuf, testutil::Repo, PathBuf) {
        let repo = testutil::Repo::new(&format!("strict-{tag}"));
        let wt = repo.start_project("foo");
        let seed = wt.join("code/seed.scan.jsonl");
        std::fs::create_dir_all(seed.parent().unwrap()).unwrap();
        std::fs::write(
            &seed,
            testutil::code_payload("fixture.mod", "/abs/store.go", &["Store"]),
        )
        .unwrap();
        {
            let r = git2::Repository::open(&wt).unwrap();
            let mut index = r.index().unwrap();
            index.add_path(Path::new("code/seed.scan.jsonl")).unwrap();
            index.write().unwrap();
            let tree_id = index.write_tree().unwrap();
            let tree = r.find_tree(tree_id).unwrap();
            let sig = r.signature().unwrap();
            let head = r.head().unwrap().peel_to_commit().unwrap();
            r.commit(Some("HEAD"), &sig, &sig, "seed code", &tree, &[&head])
                .unwrap();
        }
        testutil::scan_checkout(&wt).unwrap();
        (wt.join(specs::LAYOUT), repo, wt)
    }

    /// Phase-7 task-1 (E2E, top-level dispatch): the strict-mutation surface's
    /// refusal sweep. Every create arm — `node add`, `edge add`, `plan add`
    /// (the plan itself), and `plan add phase|task|planned` — refuses an
    /// existing entity (non-zero, error naming the `update`/`rm` follow-up, no
    /// store change); `rm` on an absent entity is non-zero, never a silent
    /// no-op.
    #[test]
    fn strict_surface_top_level_dispatch_refuses_existing_and_absent_rm() {
        let (apg_root, repo, wt) = strict_surface_fixture("dispatch-refusal");

        // --- node add refuses an existing FQN (naming update/rm) ---
        with_cwd(&wt, || {
            node_cmd::cmd_node(&argv(&["add", "requirements", "requirement", "r1"]))
        })
        .unwrap();
        let r1_path =
            layers::node_file_path(&apg_root, layers::Layer::Requirements, "requirement", "r1");
        let r1_before = std::fs::read_to_string(&r1_path).unwrap();
        let err = with_cwd(&wt, || {
            node_cmd::cmd_node(&argv(&["add", "requirements", "requirement", "r1"])).unwrap_err()
        });
        let msg = err.to_string();
        assert!(msg.contains("already exists"), "{msg}");
        assert!(msg.contains("apg node update"), "{msg}");
        assert!(msg.contains("apg node rm"), "{msg}");
        assert_eq!(
            std::fs::read_to_string(&r1_path).unwrap(),
            r1_before,
            "a refused re-add must write nothing"
        );

        // --- edge add refuses a duplicate (kind, from, to) ---
        with_cwd(&wt, || {
            node_cmd::cmd_node(&argv(&["add", "requirements", "requirement", "r2"]))
        })
        .unwrap();
        let edge = argv(&[
            "add",
            "depends-on",
            "requirements.requirement.r1",
            "requirements.requirement.r2",
        ]);
        with_cwd(&wt, || node_cmd::cmd_edge(&edge)).unwrap();
        let r1_after_edge = std::fs::read_to_string(&r1_path).unwrap();
        let err = with_cwd(&wt, || node_cmd::cmd_edge(&edge).unwrap_err());
        let msg = err.to_string();
        assert!(msg.contains("already exists"), "{msg}");
        assert!(msg.contains("apg edge update"), "{msg}");
        assert!(msg.contains("apg edge rm"), "{msg}");
        assert_eq!(
            std::fs::read_to_string(&r1_path).unwrap(),
            r1_after_edge,
            "a refused duplicate must not add a second out-half"
        );
        let r2 =
            layers::read_node_file(&apg_root, layers::Layer::Requirements, "requirement", "r2")
                .unwrap();
        assert_eq!(
            r2.in_edges.len(),
            1,
            "the duplicate must not add an in-half"
        );

        // --- rm on an absent node is non-zero (never a silent no-op) ---
        let err = with_cwd(&wt, || {
            node_cmd::cmd_node(&argv(&["rm", "requirements", "requirement", "ghost"])).unwrap_err()
        });
        assert!(!err.to_string().is_empty(), "{err}");

        // --- plan add refuses an existing plan, naming update/rm ---
        with_cwd(&wt, || plan_cmd::cmd_plan(&argv(&["add", "foo"]))).unwrap();
        let plan_path = specs::plan_jsonl_path(&apg_root, "foo");
        let plan_before = std::fs::read_to_string(&plan_path).unwrap();
        let err = with_cwd(&wt, || {
            plan_cmd::cmd_plan(&argv(&["add", "foo"])).unwrap_err()
        });
        let msg = err.to_string();
        assert!(msg.contains("already exists"), "{msg}");
        assert!(msg.contains("apg plan update foo"), "{msg}");
        assert!(msg.contains("apg plan rm foo"), "{msg}");
        assert_eq!(
            std::fs::read_to_string(&plan_path).unwrap(),
            plan_before,
            "a refused plan re-add must leave the store untouched"
        );

        // --- plan add phase refuses an existing phase, naming update/rm ---
        with_cwd(&wt, || {
            plan_cmd::cmd_plan(&argv(&[
                "add",
                "foo",
                "phase",
                "1",
                "--title",
                "P1",
                "--deliverable",
                "D",
            ]))
        })
        .unwrap();
        let phase_before = std::fs::read_to_string(&plan_path).unwrap();
        let err = with_cwd(&wt, || {
            plan_cmd::cmd_plan(&argv(&[
                "add",
                "foo",
                "phase",
                "1",
                "--title",
                "P1b",
                "--deliverable",
                "D",
            ]))
            .unwrap_err()
        });
        let msg = err.to_string();
        assert!(msg.contains("already exists"), "{msg}");
        assert!(msg.contains("apg plan update foo phase 1"), "{msg}");
        assert!(msg.contains("apg plan rm foo phase 1"), "{msg}");
        assert_eq!(std::fs::read_to_string(&plan_path).unwrap(), phase_before);

        // --- plan add task refuses an existing task, naming update/rm ---
        with_cwd(&wt, || {
            plan_cmd::cmd_plan(&argv(&[
                "add",
                "foo",
                "task",
                "1",
                "1",
                "--title",
                "T",
                "--kind",
                "source",
                "--verb",
                "creates",
                "--fqn",
                "/todo/new.ts",
            ]))
        })
        .unwrap();
        let task_before = std::fs::read_to_string(&plan_path).unwrap();
        let err = with_cwd(&wt, || {
            plan_cmd::cmd_plan(&argv(&[
                "add",
                "foo",
                "task",
                "1",
                "1",
                "--title",
                "T2",
                "--kind",
                "source",
                "--verb",
                "creates",
                "--fqn",
                "/todo/new.ts",
            ]))
            .unwrap_err()
        });
        let msg = err.to_string();
        assert!(msg.contains("already exists"), "{msg}");
        assert!(msg.contains("apg plan update foo task 1 1"), "{msg}");
        assert!(msg.contains("apg plan rm foo task 1 1"), "{msg}");
        assert_eq!(std::fs::read_to_string(&plan_path).unwrap(), task_before);

        // --- plan add planned refuses an existing planned node, naming update/rm ---
        with_cwd(&wt, || {
            plan_cmd::cmd_plan(&argv(&[
                "add",
                "foo",
                "planned",
                "file",
                "/todo/new.ts",
                "--name",
                "new.ts",
                "--parent",
                "fixture.mod",
            ]))
        })
        .unwrap();
        let planned_before = std::fs::read_to_string(&plan_path).unwrap();
        let err = with_cwd(&wt, || {
            plan_cmd::cmd_plan(&argv(&[
                "add",
                "foo",
                "planned",
                "file",
                "/todo/new.ts",
                "--name",
                "new.ts",
                "--parent",
                "fixture.mod",
            ]))
            .unwrap_err()
        });
        let msg = err.to_string();
        assert!(msg.contains("already exists"), "{msg}");
        assert!(
            msg.contains("apg plan update foo planned /todo/new.ts"),
            "{msg}"
        );
        assert!(
            msg.contains("apg plan rm foo planned /todo/new.ts"),
            "{msg}"
        );
        assert_eq!(std::fs::read_to_string(&plan_path).unwrap(), planned_before);

        // --- rm on absent plan entities is non-zero ---
        let err = with_cwd(&wt, || {
            plan_cmd::cmd_plan(&argv(&["rm", "foo", "phase", "9"])).unwrap_err()
        });
        assert!(err.to_string().contains("no phase 9"), "{err}");
        let err = with_cwd(&wt, || {
            plan_cmd::cmd_plan(&argv(&["rm", "ghost"])).unwrap_err()
        });
        assert!(
            err.to_string().contains("no plan for project `ghost`"),
            "{err}"
        );

        testutil::remove(&repo);
    }

    /// Phase-7 task-7 (gate): the `apg --help` text documents the strict
    /// `add|update|rm` surface for node/edge/plan, no longer names the retired
    /// `plan init`/`plan link` verbs, and leaves the `apg review` line
    /// unchanged.
    #[test]
    fn help_text_documents_strict_surface_and_preserves_review() {
        let help = help_text();
        // node/edge/plan all document add|update|rm.
        assert!(help.contains("apg node <sub>"), "node block present");
        assert!(help.contains("apg edge <sub>"), "edge block present");
        assert!(help.contains("apg session <sub>"), "session block present");
        assert!(
            help.contains("single-writer coordinator"),
            "the session block documents the coordinator"
        );
        assert!(help.contains("apg plan <sub>"), "plan block present");
        assert!(
            help.contains("add/update/rm/done/undone/note/complete/render/verify"),
            "the plan subcommand surface names add/update/rm progress verbs"
        );
        assert!(
            help.contains("add/update/rm (type-as-argument"),
            "the node surface names add/update/rm"
        );
        assert!(
            help.contains("add/update/rm (kind/from/to"),
            "the edge surface names add/update/rm"
        );
        // The retired plan verbs are gone.
        assert!(!help.contains("plan init"), "plan init is retired: {help}");
        assert!(!help.contains("plan link"), "plan link is retired: {help}");
        // The review block is untouched: the same header + the exact five-verb
        // dispatch line.
        let review_block = help
            .lines()
            .skip_while(|l| !l.contains("apg review <sub>"))
            .take(2)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            review_block.contains("Writer↔reviewer feedback cycle:"),
            "the apg review header must be unchanged: {review_block}"
        );
        assert!(
            review_block.contains("add/action/resolve/reject/list"),
            "the apg review verbs must be unchanged: {review_block}"
        );
    }

    /// Phase-04 task-4 (acceptance): the `apg node` / `apg edge` command
    /// surface is transparent — the SAME literal forms the CLI documents appear
    /// in `help_text`, the node/edge suite tools, and the distributed agent
    /// prompts.
    ///
    /// The expected strings are PINNED here as literals, not read back from the
    /// consts: a test that compares `AGENTS`/`SUITE_TOOLS` to themselves is a
    /// tautology and would pass even after a surface drift.
    #[test]
    fn acceptance_node_edge_surface_is_transparent_and_pinned_in_help_tools_and_agents() {
        // 1. `apg --help` documents the exact command surface (literal lines).
        let help = help_text();
        for needle in [
            "apg node <sub> …",
            "apg edge <sub> …",
            "Durable node-file model mutations:",
            "add/update/rm (type-as-argument, writes apg/layers;",
            "add/update/rm (kind/from/to;",
        ] {
            assert!(
                help.contains(needle),
                "help must contain {needle:?}: {help}"
            );
        }

        // 2. The embedded suite tools carry the exact mutating command forms.
        let tool = |name: &str| -> &'static str {
            SUITE_TOOLS
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, c)| *c)
                .unwrap_or_else(|| panic!("SUITE_TOOLS must embed {name}"))
        };
        assert!(
            tool("apg_node.ts").contains("apg node add|update|rm <layer> <type> <name>"),
            "apg_node.ts must carry the pinned node command form"
        );
        assert!(
            tool("apg_edge.ts").contains("apg edge add|update|rm <kind> <from> <to>"),
            "apg_edge.ts must carry the pinned edge command form"
        );

        // 3. The distributed agent prompts: the authoring prompts carry the
        // exact command forms; the reviewer/builder prompts name the tools.
        let agent = |name: &str| -> &'static str {
            AGENTS
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, c)| *c)
                .unwrap_or_else(|| panic!("AGENTS must embed {name}"))
        };
        let pinned: &[(&str, &str)] = &[
            (
                "spec-writer.md",
                "apg node add|update|rm <layer> <type> <name> [--body …] [--property k=v]* [--unset-property k]*",
            ),
            (
                "spec-writer.md",
                "apg edge add|update|rm <kind> <from> <to> [--property k=v]* [--unset-property k]*",
            ),
            (
                "codebase-navigator.md",
                "`apg node add|update|rm` / `apg edge add|update|rm`",
            ),
            ("spec-review.md", "`apg_node`/`apg_edge`"),
            ("agent-builder.md", "`apg_node`/`apg_edge`/`apg_plan_add`"),
        ];
        for (name, needle) in pinned {
            assert!(
                agent(name).contains(needle),
                "agent prompt {name} must contain {needle:?}"
            );
        }

        // Every embedded agent prompt is scanned: none may name a retired
        // surface (the pinned forms above are the only accepted vocabulary).
        for (name, content) in AGENTS {
            for retired in ["plan init", "plan link", "apg_plan_init", "apg_plan_link"] {
                assert!(
                    !content.contains(retired),
                    "agent prompt {name} must not name the retired `{retired}`"
                );
            }
        }
    }

    /// Phase-03 task-23: with a session live, a separate routed `apg query`
    /// process returns the post-mutation state with no lock error and without
    /// waiting for the session to end. A NON-routing direct `db.lbug` open is
    /// out of contract — lbug errors rather than waiting. After `apg session
    /// end` a fresh query opens the DB directly and reads the same state.
    #[test]
    fn read_access_during_a_live_session_routes_and_after_end_reads_directly() {
        let (repo, wt, wt_apg) = testutil::project_with_db("read-access");
        let home = repo.root.join("home");
        let session = testutil::start_session_process(&wt, &home);

        // A routed mutation lands and is projected write-through.
        let add = testutil::spawn_apg(&["node", "add", "requirements", "requirement", "live"], &wt);
        assert!(
            add.status.success(),
            "{}",
            String::from_utf8_lossy(&add.stderr)
        );

        // Routed read: post-mutation state, no lock error, no wait for end.
        let query = "MATCH (n:Requirement {fqn: 'requirements.requirement.live'}) RETURN count(n)";
        let routed = testutil::spawn_apg(&["query", query], &wt);
        assert!(
            routed.status.success(),
            "a routed read must succeed while the session is live: {}",
            String::from_utf8_lossy(&routed.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&routed.stdout)
                .lines()
                .last()
                .map(str::trim),
            Some("1")
        );

        // Non-routing direct open: out of contract (errors, never waits).
        let err = match crate::artifacts::ArtifactDb::open(&wt_apg) {
            Ok(_) => {
                panic!("a non-routing direct DB open must fail while the session holds the DB")
            }
            Err(e) => format!("{e:#}"),
        };
        assert!(err.contains("Could not set lock on file"), "{err}");

        // End the session; a fresh query opens the DB directly, same state.
        let end = testutil::spawn_apg(&["session", "end"], &wt);
        assert!(
            end.status.success(),
            "{}",
            String::from_utf8_lossy(&end.stderr)
        );
        let out = session.child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );

        let fresh = testutil::spawn_apg(&["query", query], &wt);
        assert!(
            fresh.status.success(),
            "{}",
            String::from_utf8_lossy(&fresh.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&fresh.stdout)
                .lines()
                .last()
                .map(str::trim),
            Some("1"),
            "after `session end` the direct reader sees the same state"
        );

        testutil::remove(&repo);
    }

    /// Phase-05 task-11 (e2e): metadata mutations require NO code re-scan.
    /// Durable `node add`/`node rm` and `edge add`/`edge rm`; transient `plan`
    /// add/rm (task/planned) and `review` add/action/resolve — each observed by
    /// a NEW `apg query` process. The "no scan invoked" evidence is concrete:
    /// `db.lbug`'s inode never changes (a scan unlinks and recreates it), the
    /// scan's `scanned_at` scan-meta is never restamped (a scan writes a new
    /// timestamp; the mutation re-anchor preserves it), and the scan pipeline's
    /// `apg-frontend.log` is never recreated.
    #[test]
    fn metadata_mutations_are_immediately_queryable_without_a_scan() {
        use std::os::unix::fs::MetadataExt;

        let (repo, wt, wt_apg) = testutil::project_with_db("no-rescan");
        let home = repo.root.join("home");

        let db_path = wt_apg.join(specs::TRANS).join("db.lbug");
        let graph_path = wt_apg.join(specs::TRANS).join("graph.jsonl");
        let inode_before = std::fs::metadata(&db_path).unwrap().ino();
        let scanned_at = |graph: &Path| -> String {
            let text = std::fs::read_to_string(graph).unwrap();
            let first = text.lines().next().unwrap_or_default().to_string();
            serde_json::from_str::<serde_json::Value>(&first)
                .ok()
                .and_then(|v| {
                    v.get("scanned_at")
                        .and_then(|s| s.as_str())
                        .map(str::to_string)
                })
                .unwrap_or_default()
        };
        let scan_meta_before = scanned_at(&graph_path);
        // A scan would recreate this; a metadata mutation never enters the scan
        // pipeline.
        let _ = std::fs::remove_file(wt_apg.join(specs::TRANS).join("apg-frontend.log"));

        let mutate = |args: &[&str]| {
            let out = testutil::ApgCommand::new(args)
                .cwd(&wt)
                .env("HOME", home.to_str().unwrap())
                .output();
            assert!(
                out.status.success(),
                "{args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        let query = |q: &str| -> String {
            let out = testutil::spawn_apg(&["query", q], &wt);
            assert!(
                out.status.success(),
                "{q}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .last()
                .map(|l| l.trim().to_string())
                .unwrap_or_default()
        };

        // --- durable node add/rm + edge add/rm ---
        mutate(&["node", "add", "requirements", "requirement", "r1"]);
        assert_eq!(
            query("MATCH (n:Requirement {fqn: 'requirements.requirement.r1'}) RETURN count(n)"),
            "1"
        );
        mutate(&["node", "add", "requirements", "requirement", "r2"]);
        assert_eq!(
            query("MATCH (n:Requirement {fqn: 'requirements.requirement.r2'}) RETURN count(n)"),
            "1"
        );
        mutate(&[
            "edge",
            "add",
            "depends-on",
            "requirements.requirement.r1",
            "requirements.requirement.r2",
        ]);
        assert_eq!(
            query(
                "MATCH (:Requirement {fqn: 'requirements.requirement.r1'})-[:DependsOn]->(:Requirement {fqn: 'requirements.requirement.r2'}) RETURN count(*)"
            ),
            "1"
        );
        mutate(&[
            "edge",
            "rm",
            "depends-on",
            "requirements.requirement.r1",
            "requirements.requirement.r2",
        ]);
        assert_eq!(
            query(
                "MATCH (:Requirement {fqn: 'requirements.requirement.r1'})-[:DependsOn]->() RETURN count(*)"
            ),
            "0"
        );
        mutate(&["node", "rm", "requirements", "requirement", "r1"]);
        assert_eq!(
            query("MATCH (n:Requirement {fqn: 'requirements.requirement.r1'}) RETURN count(n)"),
            "0"
        );

        // --- transient plan: phase/task/planned add + rm ---
        mutate(&["plan", "add", "foo", "--title", "F", "--strategy", "S"]);
        assert_eq!(
            query("MATCH (p:Plan {fqn: 'foo/plan'}) RETURN count(p)"),
            "1"
        );
        mutate(&[
            "plan",
            "add",
            "foo",
            "phase",
            "1",
            "--title",
            "P1",
            "--deliverable",
            "D",
        ]);
        assert_eq!(
            query("MATCH (n:PlanPhase {fqn: 'foo/plan.phase-01'}) RETURN count(n)"),
            "1"
        );
        mutate(&[
            "plan",
            "add",
            "foo",
            "planned",
            "struct",
            "fixture.mod.Widget",
            "--name",
            "Widget",
            "--parent",
            "fixture.mod",
        ]);
        assert_eq!(
            query("MATCH (n:Struct {fqn: 'fixture.mod.Widget'}) RETURN count(n)"),
            "1"
        );
        mutate(&["plan", "rm", "foo", "planned", "fixture.mod.Widget"]);
        assert_eq!(
            query("MATCH (n:Struct {fqn: 'fixture.mod.Widget'}) RETURN count(n)"),
            "0"
        );
        mutate(&[
            "plan",
            "add",
            "foo",
            "task",
            "1",
            "1",
            "--title",
            "T1",
            "--verb",
            "modifies",
            "--fqn",
            "fixture.mod.Store",
        ]);
        assert_eq!(
            query("MATCH (n:Task {fqn: 'foo/plan.phase-01.task-1'}) RETURN count(n)"),
            "1"
        );
        mutate(&["plan", "rm", "foo", "task", "1", "1"]);
        assert_eq!(
            query("MATCH (n:Task {fqn: 'foo/plan.phase-01.task-1'}) RETURN count(n)"),
            "0"
        );

        // --- transient review: add/action/resolve ---
        mutate(&["node", "add", "requirements", "requirement", "reviewed"]);
        mutate(&[
            "review",
            "add",
            "requirements.requirement.reviewed",
            "--body",
            "please fix",
            "--project",
            "foo",
        ]);
        assert_eq!(
            query("MATCH (f:Feedback {fqn: 'foo/feedback-1'}) RETURN count(f)"),
            "1"
        );
        mutate(&["review", "action", "foo/feedback-1", "--fix"]);
        assert_eq!(
            query("MATCH (f:Feedback {fqn: 'foo/feedback-1'}) RETURN f.status"),
            "actioned"
        );
        mutate(&["review", "resolve", "foo/feedback-1"]);
        assert_eq!(
            query("MATCH (f:Feedback {fqn: 'foo/feedback-1'}) RETURN f.status"),
            "resolved"
        );

        // --- no re-scan: projection inode, scan_meta, and the scan log are untouched ---
        assert_eq!(
            std::fs::metadata(&db_path).unwrap().ino(),
            inode_before,
            "db.lbug must never be re-created by a metadata mutation"
        );
        assert_eq!(
            scanned_at(&graph_path),
            scan_meta_before,
            "a metadata mutation must never restamp the scan_meta (no scan ran)"
        );
        assert!(
            !wt_apg.join(specs::TRANS).join("apg-frontend.log").exists(),
            "a metadata mutation must not enter the scan pipeline"
        );

        testutil::remove(&repo);
    }

    /// Phase-04 task-7 (acceptance, feedback-30): immediate queryability /
    /// read-your-writes across the real CLI. A spawned `apg node add
    /// requirements requirement foo` returns, then a NEW `apg query` process
    /// resolves foo — no re-scan, no explicit flush, no session-end step — and
    /// the test asserts NO `apg scan` was invoked: `db.lbug`'s inode never
    /// changes (a scan unlinks and recreates it), the scan's `scanned_at`
    /// scan-meta is never restamped, and the scan pipeline's
    /// `apg-frontend.log` is never recreated.
    #[test]
    fn acceptance_read_your_writes_across_the_real_cli_without_a_scan() {
        use std::os::unix::fs::MetadataExt;

        let (repo, wt, wt_apg) = testutil::project_with_db("accept-ryw");
        let home = repo.root.join("home");

        let db_path = wt_apg.join(specs::TRANS).join("db.lbug");
        let graph_path = wt_apg.join(specs::TRANS).join("graph.jsonl");
        let log_path = wt_apg.join(specs::TRANS).join("apg-frontend.log");
        let scanned_at = |graph: &Path| -> String {
            let text = std::fs::read_to_string(graph).unwrap();
            let first = text.lines().next().unwrap_or_default().to_string();
            serde_json::from_str::<serde_json::Value>(&first)
                .ok()
                .and_then(|v| {
                    v.get("scanned_at")
                        .and_then(|s| s.as_str())
                        .map(str::to_string)
                })
                .unwrap_or_default()
        };
        let inode_before = std::fs::metadata(&db_path).unwrap().ino();
        let scan_meta_before = scanned_at(&graph_path);
        // A scan would recreate this; a metadata mutation never enters the scan
        // pipeline.
        let _ = std::fs::remove_file(&log_path);

        // (1) `apg node add requirements requirement foo` returns.
        let add = testutil::ApgCommand::new(&["node", "add", "requirements", "requirement", "foo"])
            .cwd(&wt)
            .env("HOME", home.to_str().unwrap())
            .output();
        assert!(
            add.status.success(),
            "{}",
            String::from_utf8_lossy(&add.stderr)
        );

        // (2) A NEW `apg query` process resolves foo — no flush/session-end.
        let q = testutil::spawn_apg(
            &[
                "query",
                "MATCH (n:Requirement {fqn: 'requirements.requirement.foo'}) RETURN count(n)",
            ],
            &wt,
        );
        assert!(q.status.success(), "{}", String::from_utf8_lossy(&q.stderr));
        assert_eq!(
            String::from_utf8_lossy(&q.stdout)
                .lines()
                .last()
                .map(str::trim),
            Some("1"),
            "a NEW process must read the mutation immediately"
        );

        // (3) No `apg scan` was invoked.
        assert_eq!(
            std::fs::metadata(&db_path).unwrap().ino(),
            inode_before,
            "db.lbug must never be re-created by a metadata mutation"
        );
        assert_eq!(
            scanned_at(&graph_path),
            scan_meta_before,
            "a metadata mutation must never restamp the scan_meta (no scan ran)"
        );
        assert!(
            !log_path.exists(),
            "a metadata mutation must not enter the scan pipeline"
        );

        testutil::remove(&repo);
    }

    /// Initializes a bare scratch git repo at `dir` (branch `main`) with the
    /// test identity configured — the fresh NON-FIXTURE target of the
    /// external-project acceptance (no `apg/` layout: the real `apg init` is
    /// part of the test).
    fn scratch_repo_init(dir: &Path) -> git2::Repository {
        std::fs::create_dir_all(dir).unwrap();
        let mut opts = git2::RepositoryInitOptions::new();
        opts.initial_head("refs/heads/main");
        let repo = git2::Repository::init_opts(dir, &opts).unwrap();
        {
            let mut cfg = repo.config().unwrap();
            cfg.set_str("user.name", "apg scratch test").unwrap();
            cfg.set_str("user.email", "apg-scratch@example.com")
                .unwrap();
        }
        repo
    }

    /// Commits every change under `dir` (git2 — the git CLI is never shelled
    /// out to anywhere in src). Tolerates an unborn HEAD (the first commit).
    fn scratch_commit_all(dir: &Path, msg: &str) {
        let repo = git2::Repository::open(dir).unwrap();
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = repo.signature().unwrap();
        let head = repo.head().ok().map(|h| h.peel_to_commit().unwrap());
        let parents: Vec<&git2::Commit> = head.iter().collect();
        repo.commit(Some("HEAD"), &sig, &sig, msg, &tree, parents.as_slice())
            .unwrap();
    }

    /// Phase-04 task-8 (acceptance): the hermetic external-project scratch-repo
    /// acceptance — a FRESH NON-FIXTURE repo driven by a REAL `apg init` + REAL
    /// `apg scan` (the genuine added dimension; every other test uses the
    /// hermetic `scan_checkout` payload fixture).
    ///
    /// The already-built artifact is resolved through the `apg.testutil`
    /// binary-locating helper (`ApgCommand`) — no nested `cargo build`. Each
    /// spawned child gets its own isolated `HOME` via `Command::env` (edition
    /// 2024 forbids process-wide `env::set_var`, and `apg init` installs the
    /// suite into `$HOME/.opencode`). A real source file is committed BEFORE
    /// scanning so `auto_detect_languages` selects just the Go frontend. The
    /// project context is established with `apg project start`, and every
    /// durable-write assertion runs with cwd inside
    /// `<scratch>/apg/.worktrees/<name>`. Both the `/tmp` scratch repo and the
    /// isolated HOME are torn down at the end.
    #[test]
    fn acceptance_scratch_repo_real_init_scan_burst_read_your_writes_and_session() {
        let base = std::env::temp_dir().join(format!("apg-accept-scratch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let repo_dir = base.join("repo");
        let home = base.join("home");
        std::fs::create_dir_all(&home).unwrap();
        // Pre-create the opencode dependency dir so `apg init` never shells out
        // to npm (`cmd_init` skips npm when this path already exists) — keeps
        // the test hermetic and fast.
        std::fs::create_dir_all(home.join(".opencode/node_modules/@opencode-ai/plugin")).unwrap();
        let home_s = home.to_str().unwrap().to_string();

        // A fresh real git repo with a committed Go source file + manifest
        // BEFORE scanning.
        scratch_repo_init(&repo_dir);
        std::fs::write(repo_dir.join("go.mod"), "module scratch\n\ngo 1.21\n").unwrap();
        std::fs::write(repo_dir.join("main.go"), "package main\n\nfunc main() {}\n").unwrap();
        scratch_commit_all(&repo_dir, "init source");

        let repo_dir_s = repo_dir.clone();
        let home_for = home_s.clone();
        let run_in = move |dir: &Path, args: &[&str]| {
            let out = testutil::ApgCommand::new(args)
                .cwd(dir)
                .env("HOME", &home_for)
                .output();
            assert!(
                out.status.success(),
                "{args:?} in {}: {}{}",
                dir.display(),
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            out
        };

        // Real `apg init` in the scratch main checkout, then commit the scaffold
        // (so the main checkout is clean for `project start`).
        run_in(&repo_dir_s, &["init", "."]);
        scratch_commit_all(&repo_dir, "apg init");

        // Real `apg scan` in the scratch main checkout.
        run_in(&repo_dir_s, &["scan", "."]);

        // `apg project start <name>` from the main checkout: worktree + branch +
        // branch DB (auto-scanned).
        run_in(&repo_dir_s, &["project", "start", "accept"]);
        let wt = repo_dir.join("apg").join(".worktrees").join("accept");
        assert!(
            wt.is_dir(),
            "the project worktree must exist at {}",
            wt.display()
        );
        let wt_apg = wt.join(specs::LAYOUT);

        // ---- (a) cross-process burst == serial application, zero lock errors ----
        const N: usize = 6;
        run_in(&wt, &["node", "add", "requirements", "requirement", "hub"]);
        for i in 0..N {
            let name = format!("leaf-{i}");
            run_in(
                &wt,
                &["node", "add", "requirements", "requirement", name.as_str()],
            );
        }
        let home_burst = home_s.clone();
        let mut kids = Vec::with_capacity(N);
        for i in 0..N {
            let to = format!("requirements.requirement.leaf-{i}");
            let child = testutil::ApgCommand::new(&[
                "edge",
                "add",
                "depends-on",
                "requirements.requirement.hub",
                to.as_str(),
            ])
            .cwd(&wt)
            .env("HOME", &home_burst)
            .spawn();
            kids.push((i, child));
        }
        for (i, child) in kids {
            let out = child.wait_with_output().unwrap();
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert!(out.status.success(), "burst[{i}] lost a lock: {stderr}");
            assert!(
                !stderr.contains("Could not set lock on file"),
                "burst[{i}] hit the lbug lock: {stderr}"
            );
            assert!(
                !stderr.contains("index.lock"),
                "burst[{i}] hit the git index lock: {stderr}"
            );
            assert!(
                !stderr.contains("specs.lock"),
                "burst[{i}] hit the specs.lock flock: {stderr}"
            );
        }
        let hub =
            layers::read_node_file(&wt_apg, layers::Layer::Requirements, "requirement", "hub")
                .unwrap();
        assert_eq!(
            hub.out.len(),
            N,
            "the burst store must equal the serial application"
        );

        // ---- (b) immediate read-your-writes across the real CLI ----
        run_in(&wt, &["node", "add", "requirements", "requirement", "foo"]);
        let q = testutil::spawn_apg(
            &[
                "query",
                "MATCH (n:Requirement {fqn: 'requirements.requirement.foo'}) RETURN count(n)",
            ],
            &wt,
        );
        assert!(q.status.success(), "{}", String::from_utf8_lossy(&q.stderr));
        assert_eq!(
            String::from_utf8_lossy(&q.stdout)
                .lines()
                .last()
                .map(str::trim),
            Some("1"),
            "a NEW apg query process must read foo with no scan/flush"
        );

        // ---- (c) full session lifecycle: start → routed mutation/read → end → direct read ----
        let session = testutil::start_session_process(&wt, &home);
        run_in(
            &wt,
            &["node", "add", "requirements", "requirement", "routed"],
        );
        let routed = testutil::spawn_apg(
            &[
                "query",
                "MATCH (n:Requirement {fqn: 'requirements.requirement.routed'}) RETURN count(n)",
            ],
            &wt,
        );
        assert!(
            routed.status.success(),
            "{}",
            String::from_utf8_lossy(&routed.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&routed.stdout)
                .lines()
                .last()
                .map(str::trim),
            Some("1"),
            "a routed read must see the routed mutation before session end"
        );
        let end = testutil::spawn_apg(&["session", "end"], &wt);
        assert!(
            end.status.success(),
            "{}",
            String::from_utf8_lossy(&end.stderr)
        );
        let sout = session.child.wait_with_output().unwrap();
        assert!(
            sout.status.success(),
            "{}",
            String::from_utf8_lossy(&sout.stderr)
        );

        // After `session end`, a direct read sees the same state, and db.lbug is
        // consistent with the durable node files (every requirement node file
        // has its row; the row count matches).
        let direct = testutil::spawn_apg(
            &[
                "query",
                "MATCH (n:Requirement {fqn: 'requirements.requirement.routed'}) RETURN count(n)",
            ],
            &wt,
        );
        assert!(
            direct.status.success(),
            "{}",
            String::from_utf8_lossy(&direct.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&direct.stdout)
                .lines()
                .last()
                .map(str::trim),
            Some("1"),
            "after session end the direct reader sees the routed mutation"
        );
        {
            let db = crate::artifacts::ArtifactDb::open(&wt_apg).unwrap();
            let node_files = layers::read_existing_nodes(&wt_apg).unwrap();
            let requirements: Vec<_> = node_files
                .iter()
                .filter(|n| n.layer == "requirements" && n.node_type == "requirement")
                .collect();
            for n in &requirements {
                let f = layers::fqn(layers::Layer::Requirements, &n.node_type, &n.name);
                assert!(db.has_node(&f), "db.lbug must be consistent with {f}");
            }
            let rows = db
                .q("MATCH (n:Requirement) RETURN count(*)")
                .unwrap()
                .lines()
                .last()
                .and_then(|l| l.trim().parse::<usize>().ok())
                .unwrap_or(0);
            assert_eq!(
                rows,
                requirements.len(),
                "db.lbug Requirement rows must match the node files"
            );
        }

        // ---- teardown: the scratch repo AND the isolated HOME ----
        let _ = std::fs::remove_dir_all(&base);
    }
}
