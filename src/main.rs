mod artifacts;
mod classify;
mod cleanup;
mod git;
mod graph;
mod ingest;
mod invariant_cmd;
mod layers;
mod load;
mod plan_cmd;
mod project_cmd;
mod review_cmd;
mod schema;
mod spec_cmd;
mod specs;
#[cfg(test)]
mod testutil;
mod version_gate;

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
    // Spec/plan/review suite (SPEC R12).
    (
        "apg_spec.ts",
        include_str!("../opencode-suite/tools/apg_spec.ts"),
    ),
    (
        "apg_spec_requirements.ts",
        include_str!("../opencode-suite/tools/apg_spec_requirements.ts"),
    ),
    (
        "apg_spec_phases.ts",
        include_str!("../opencode-suite/tools/apg_spec_phases.ts"),
    ),
    (
        "apg_spec_deps.ts",
        include_str!("../opencode-suite/tools/apg_spec_deps.ts"),
    ),
    (
        "apg_spec_anchors.ts",
        include_str!("../opencode-suite/tools/apg_spec_anchors.ts"),
    ),
    (
        "apg_spec_trace.ts",
        include_str!("../opencode-suite/tools/apg_spec_trace.ts"),
    ),
    (
        "apg_spec_unresolved.ts",
        include_str!("../opencode-suite/tools/apg_spec_unresolved.ts"),
    ),
    (
        "apg_spec_fixes.ts",
        include_str!("../opencode-suite/tools/apg_spec_fixes.ts"),
    ),
    (
        "apg_spec_init.ts",
        include_str!("../opencode-suite/tools/apg_spec_init.ts"),
    ),
    (
        "apg_spec_add.ts",
        include_str!("../opencode-suite/tools/apg_spec_add.ts"),
    ),
    (
        "apg_spec_anchor.ts",
        include_str!("../opencode-suite/tools/apg_spec_anchor.ts"),
    ),
    (
        "apg_spec_link.ts",
        include_str!("../opencode-suite/tools/apg_spec_link.ts"),
    ),
    (
        "apg_spec_spine.ts",
        include_str!("../opencode-suite/tools/apg_spec_spine.ts"),
    ),
    (
        "apg_spec_rm.ts",
        include_str!("../opencode-suite/tools/apg_spec_rm.ts"),
    ),
    (
        "apg_spec_render.ts",
        include_str!("../opencode-suite/tools/apg_spec_render.ts"),
    ),
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
        "apg_invariant_add.ts",
        include_str!("../opencode-suite/tools/apg_invariant_add.ts"),
    ),
    (
        "apg_invariants.ts",
        include_str!("../opencode-suite/tools/apg_invariants.ts"),
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
        "apg_plan_init.ts",
        include_str!("../opencode-suite/tools/apg_plan_init.ts"),
    ),
    (
        "apg_plan_add.ts",
        include_str!("../opencode-suite/tools/apg_plan_add.ts"),
    ),
    (
        "apg_plan_link.ts",
        include_str!("../opencode-suite/tools/apg_plan_link.ts"),
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
        "apg_plan_apply.ts",
        include_str!("../opencode-suite/tools/apg_plan_apply.ts"),
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

fn print_help() {
    println!(
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
  apg spec <sub> …            Author + lifecycle a graph-native spec:
                              init/add/anchor/link/spine/rm/render/unresolved
                              (add authors the 4-tier taxonomy: requirement,
                              phase, decision, non-goal, AC, VI, note,
                              stakeholder, domain, subdomain, entity,
                              value-object, aggregate, domain-event,
                              domain-process, domain-rule, actor, system,
                              container, component)
  apg plan <sub> …            The phased execution plan (transient, branch-local):
                              init/add/link/done/undone/note/complete/render/verify
                              (add authors phases, tasks, and planned
                              Implementation nodes — module/file/struct/function
                              marked planned at the FQN where the code lands;
                              verify is the pre-merge coherence gate)
  apg project <sub> …         Project contexts (worktrees, git2-operated):
                              start <name> — worktree + branch + branch DB off
                              the default branch (apg/.worktrees/<name>);
                              merge <name> — verify gate → merge → main rebuild
  apg review <sub> …          Writer↔reviewer feedback cycle:
                              add/action/resolve/reject/list
  apg invariant add …         Materialize a graph-wide invariant (universal or
                              project-scoped), optionally guarding artifacts
  apg invariants              List invariants (filter by --scope/--project)
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
        "spec" => spec_cmd::cmd_spec(&raw[2..]),
        "plan" => plan_cmd::cmd_plan(&raw[2..]),
        "review" => review_cmd::cmd_review(&raw[2..]),
        "project" => project_cmd::cmd_project(&raw[2..]),
        "invariant" => invariant_cmd::cmd_invariant(&raw[2..]),
        "invariants" => invariant_cmd::cmd_invariants(&raw[2..]),
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
    let pruned = prune_stale_suite(&opencode_dir)?;

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
    let db_path = apg_root.join(specs::TRANS).join("db.lbug");
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
    if json {
        println!("{}", emit_json_rows(result));
    } else {
        let names = result.get_column_names();
        let header: Vec<String> = names.iter().map(|n| csv_escape(n)).collect();
        println!("{}", header.join(","));
        for row in result {
            let cells: Vec<String> = row.iter().map(|v| csv_escape(&v.to_string())).collect();
            println!("{}", cells.join(","));
        }
    }
    Ok(())
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
    let iterators: Vec<Box<dyn Iterator<Item = schema::Record>>> = spools
        .into_iter()
        .map(|(lang, spool)| {
            let lines = BufReader::new(std::fs::File::open(&spool).unwrap()).lines();
            let records = lines.map(|x| {
                let line = x.expect("io error");
                serde_json::from_str::<schema::Record>(&line)
                    .unwrap_or_else(|e| panic!("bad json {e}: {line}"))
            });
            Box::new(std::iter::once(schema::Record::LangSwitch { language: lang }).chain(records))
                as Box<dyn Iterator<Item = schema::Record>>
        })
        .collect();
    let records = iterators.into_iter().flatten();
    let records = std::iter::once(schema::Record::ScanMeta {
        git_sha: git_state.sha.clone(),
        git_clean: git_state.sha.as_ref().map(|_| git_state.clean),
        scanned_at: git::now_iso8601(),
    })
    .chain(records);

    // Re-ingest the committed spec/plan/note data after code (SPEC R10):
    // `apg/specs/*.jsonl`, `apg/notes/*.jsonl`, `apg/.trans/plans/*.jsonl`.
    // Spec records carry canonical FQNs and reference code by FQN, so they
    // merge into the same stream; pending anchors are reconciled ingestor-side.
    let spec_inputs = specs::scan_inputs(&apg_root);
    let mut spec_count = 0;
    for set in [
        spec_inputs.0.clone(),
        spec_inputs.1.clone(),
        spec_inputs.2.clone(),
    ] {
        spec_count += set.len();
    }
    if spec_count > 0 {
        log.ln(&format!(
            "Spec/plan/note inputs: {} spec files, {} note files, {} plan files",
            spec_inputs.0.len(),
            spec_inputs.1.len(),
            spec_inputs.2.len(),
        ));
    }
    let records = records.chain(specs::read_all(&apg_root));

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
    const RELEASE_VERSION: &str = "0.10.4";

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
        // The README pins the 0.10.x line, not an exact patch, so patch releases
        // don't require a README edit.
        assert!(readme.contains("apg 0.10.x"), "README --version examples");
        assert!(readme.contains("0.10.x"), "README tagged-release prose");
        assert!(
            readme.contains("--version 0.10.x"),
            "README Linux installer pin option"
        );
        // No stale release records: the previous version must be fully replaced.
        assert!(!readme.contains("0.9.3"), "README must not reference 0.9.3");
    }
}
