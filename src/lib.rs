//! The `apg` library: the root crate's production module tree and its
//! crate-internal API. The binary (`src/main.rs`) is a thin wrapper that
//! dispatches into the library's entry point; the inline white-box tests live
//! here with the modules they exercise.
//!
//! Every module is public so integration crates (and the relocated test tiers)
//! can reach the internals they exercise. `testutil` is the shared test
//! harness and compiles unconditionally as a public module.

pub mod artifacts;
pub mod cache;
pub mod classify;
pub mod cleanup;
pub mod delta;
pub mod git;
pub mod graph;
pub mod impact;
pub mod incremental;
pub mod ingest;
pub mod layers;
pub mod load;
pub mod node_cmd;
pub mod plan_cmd;
pub mod project_cmd;
pub mod review_cmd;
pub mod schema;
pub mod session;
pub mod specs;
pub mod splice;
pub mod testutil;
pub mod timing;
pub mod version_gate;

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
pub const SUITE_TOOLS: &[(&str, &str)] = &[
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
pub const APG_LIB: &str = include_str!("../opencode-suite/lib/apg.ts");

/// The layout-upgrade guide that `apg init` installs into
/// `~/.opencode/lib/apg-upgrade.md` next to the shared lib: version-field
/// meaning, mismatch detection, upgrade steps (re-run init → re-scan),
/// common-issue fixes. The R10 version gate's block text points at it.
///
/// Maintained inline here; if the suite tree (`opencode-suite/lib/`) ever
/// gains an `apg-upgrade.md`, this const should flip to an `include_str!` of
/// it like `APG_LIB` above.
pub const APG_UPGRADE_DOC: &str = r#"# apg layout upgrades — the `apg/config.json` version field

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
pub const AGENTS: &[(&str, &str)] = &[
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
    "@opencode-ai/plugin": "1.18.20"
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
pub struct Log {
    f: std::fs::File,
}

impl Log {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Log {
        Log {
            f: std::fs::File::create("apg-frontend.log")
                .expect("failed to create apg-frontend.log"),
        }
    }

    pub fn ln(&mut self, msg: &str) {
        eprintln!("{msg}");
        let _ = writeln!(self.f, "{msg}");
    }

    /// Appends a spooled file's contents to the log only (no terminal echo),
    /// used to fold a frontend's captured stderr into `apg-frontend.log`.
    pub fn append_file(&mut self, path: &Path) {
        if let Ok(content) = std::fs::read_to_string(path) {
            let _ = write!(self.f, "{content}");
        }
    }
}

/// Emits the per-phase timing report first-class (phase-04 task-1/task-2): the
/// human `[timing]` line plus the machine-readable `[timing-json]` line, both
/// through the scan log (stderr *and* `apg-frontend.log`).
fn emit_timing(log: &mut Log, report: &timing::TimingReport) {
    log.ln(&report.human_line());
    log.ln(&report.machine_line());
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

/// The bundled structural scanner's stream ids, in canonical order: Markdown
/// plus the per-format structural streams and the residual `misc`. ONE
/// `structfrontend` binary serves all of them; the driver injects one
/// `lang_switch` per id and spawns one tool-less stream per id (the
/// per-stream `--stream <id>` selector). They are NOT extension-detectable —
/// `misc` has no extension and `md` alone is not the bundle — so the driver
/// enumerates them whenever `structfrontend` is installed.
const STRUCTURAL_LANGUAGES: &[&str] = &[
    "md",
    "sh",
    "yaml",
    "json",
    "toml",
    "xml",
    "dockerfile",
    "makefile",
    "ini",
    "misc",
];

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
        if dir.join("structfrontend").exists() {
            // The bundled structural scanner: one binary serving the `md` stream
            // plus every per-format structural stream and the residual `misc`,
            // listed in canonical order.
            langs.extend(STRUCTURAL_LANGUAGES.iter().map(|s| s.to_string()));
        }
        if dir.join("pyfrontend").exists() {
            langs.push("py".into());
        }
        if dir.join("java-classes").is_dir() {
            langs.push("java".into());
        }
        if dir.join("tsfrontend").is_dir() {
            // One unified JS/TS frontend artifact, two language ids: a JS-only
            // repo auto-detects and scans without a separate JS frontend install.
            langs.push("ts".into());
            langs.push("js".into());
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
            "md" | "sh" | "yaml" | "json" | "toml" | "xml" | "dockerfile" | "makefile" | "ini"
            | "misc"
                if dir.join("structfrontend").exists() =>
            {
                // ONE binary serves every structural stream id. `frontend_cmd`
                // returns ONLY the command line: the per-stream `--stream <id>`
                // selector is appended at spawn time by `spawn_frontend`, the
                // single owner of that flag.
                return Some(dir.join("structfrontend").display().to_string());
            }
            "py" if dir.join("pyfrontend").exists() => {
                return Some(dir.join("pyfrontend").display().to_string());
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
                    "java -Xmx5g -cp {} CallGraphBuilder",
                    classes.display()
                ));
            }
            "ts" | "js" if dir.join("tsfrontend").is_dir() => {
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
        // The bundled structural scanner serves every structural stream id from
        // one baked artifact (`build.rs` stages `structfrontend`).
        "md" | "sh" | "yaml" | "json" | "toml" | "xml" | "dockerfile" | "makefile" | "ini"
        | "misc" => option_env!("APG_FRONTEND_STRUCT"),
        "py" => option_env!("APG_FRONTEND_PY"),
        "csharp" => option_env!("APG_FRONTEND_CSHARP"),
        "java" => option_env!("APG_FRONTEND_JAVA"),
        // One built artifact, two ids: `js`-only repos run the same unified
        // JS/TS frontend under the `js` id.
        "ts" | "js" => option_env!("APG_FRONTEND_TS"),
        _ => None,
    };
    baked.map(|s| s.to_string())
}

/// The default detector skip set: directory names the language walk never
/// descends into, on top of the walker's unconditional hidden dot-name rule.
/// Preserves the detector's long-standing behaviour (`target` +
/// `node_modules`).
const DEFAULT_DETECTOR_SKIP: &[&str] = &["target", "node_modules"];

/// The markdown detector's skip set — `domain.entity.scan-exclusion`'s
/// `target`/`vendor`/`node_modules`/`.worktrees`. `.worktrees` is already
/// covered by the walker's hidden dot-name rule; it is listed for fidelity
/// with the spec set. Markdown under an excluded tree must not trigger md
/// detection.
const MD_DETECTOR_SKIP: &[&str] = &["target", "vendor", "node_modules", ".worktrees"];

/// The Python detector's skip set — `domain.constraint.python-exclusions`'
/// non-hidden entries plus the default `target`/`node_modules`. Neither
/// `__pycache__`, `venv`, `site-packages` nor `*.egg-info` is dot-prefixed, so
/// the walker's hidden-name rule does not cover them and they MUST be listed;
/// the constraint's hidden entries (`.venv`, `.tox`, `.git`, `.hg`,
/// `.mypy_cache`, `.pytest_cache`, `.ruff_cache`, `.eggs`) are already skipped
/// by that rule and are deliberately not listed. `*.egg-info` is a glob the
/// walker matches against the directory name (literals match exactly).
const PY_DETECTOR_SKIP: &[&str] = &[
    "target",
    "node_modules",
    "__pycache__",
    "venv",
    "site-packages",
    "*.egg-info",
];

/// True when `dir` holds, within `depth` directory levels, a file whose
/// extension is one of `exts` (each with its leading dot).
///
/// `skip_set` names directories never descended into. Each entry is matched
/// against a directory's file name: a literal entry (`target`, `vendor`, …)
/// matches that exact name, and an entry containing glob metacharacters
/// (`*.egg-info`) is matched as a glob against the name. The walker's
/// unconditional hidden dot-name rule (`name.starts_with('.')`) stays in force
/// in addition to the set, so callers pass only the non-hidden exclusions they
/// need. The exclusion set — not `depth` — is the safety bound: an excluded
/// dependency/generated tree is never entered.
fn has_extension(dir: &std::path::Path, exts: &[&str], depth: u32, skip_set: &[&str]) -> bool {
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
        if name.starts_with('.') {
            continue;
        }
        if p.is_dir() {
            if skip_set.iter().any(|s| classify::matches_glob(s, name)) {
                continue;
            }
            if has_extension(&p, exts, depth - 1, skip_set) {
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
pub fn auto_detect_languages(dir: &std::path::Path, available: &[String]) -> Vec<String> {
    // C++ is derived from the root crate's single source of truth
    // (`incremental::CPP_EXTENSIONS`, dotless) so detection cannot drift from
    // `language_of`'s classification or the frontend's `is_cpp_ext`.
    let cpp_dotted: Vec<String> = incremental::CPP_EXTENSIONS
        .iter()
        .map(|e| format!(".{e}"))
        .collect();
    let cpp_exts: Vec<&str> = cpp_dotted.iter().map(String::as_str).collect();
    // (language, accepted extensions, walk depth, skip set). Every candidate
    // shares the ONE generalised `has_extension` walker; each names only the
    // non-hidden directories it must not descend into (`skip_set`), and the
    // depth bound is explicit per candidate rather than a shared magic literal.
    let candidates: Vec<(&str, &[&str], u32, &[&str])> = vec![
        ("java", &[".java"] as &[&str], 5, DEFAULT_DETECTOR_SKIP),
        ("go", &[".go"], 5, DEFAULT_DETECTOR_SKIP),
        ("cpp", cpp_exts.as_slice(), 5, DEFAULT_DETECTOR_SKIP),
        ("rust", &[".rs"], 5, DEFAULT_DETECTOR_SKIP),
        (
            "ts",
            &[".ts", ".tsx", ".mts", ".cts"],
            5,
            DEFAULT_DETECTOR_SKIP,
        ),
        (
            "js",
            &[".js", ".jsx", ".mjs", ".cjs"],
            5,
            DEFAULT_DETECTOR_SKIP,
        ),
        ("csharp", &[".cs", ".csx"], 5, DEFAULT_DETECTOR_SKIP),
        // Markdown's skip set is `domain.entity.scan-exclusion`
        // (task-8): a markdown-only file under `vendor/` (or another excluded
        // tree) must not trigger md detection.
        ("md", &[".md", ".markdown"], 5, MD_DETECTOR_SKIP),
        // Python's skip set is `domain.constraint.python-exclusions`: a tree
        // whose only Python lives under a venv / site-packages / `*.egg-info` /
        // `__pycache__` dir must not trigger py detection. Depth 8 (above the
        // default 5): Python packages legitimately nest deeper under a `src/`
        // root, and the exclusion set — not the depth — is the safety bound, so
        // a deep excluded tree is never entered. `.pyi` is accepted alongside
        // `.py`; `.pyx` is not a candidate.
        ("py", &[".py", ".pyi"], 8, PY_DETECTOR_SKIP),
    ];
    let mut out = Vec::new();
    for (lang, exts, depth, skip) in &candidates {
        if available.iter().any(|l| l == lang) && has_extension(dir, exts, *depth, skip) {
            out.push(lang.to_string());
        }
    }
    // A repo that also contains incidental JavaScript still scans ONCE: the
    // unified JS/TS frontend runs under the `ts` id, never js+ts as two
    // frontends. `has_extension` never descends into `node_modules`, so a
    // dependency tree's JS can never trigger `js` detection.
    if out.iter().any(|l| l == "ts") {
        out.retain(|l| l != "js");
    }
    // The bundled structural scanner is NOT extension-detectable: `misc` has no
    // extension and `md` alone is not the bundle, so whenever its
    // `structfrontend` is installed the structural stream ids are selected as a
    // whole, in canonical order, after the code languages. The `md` candidate
    // above is subsumed by this bundle (dropped here, re-added below).
    out.retain(|l| !classify::is_structural_language(l));
    for lang in STRUCTURAL_LANGUAGES {
        if available.iter().any(|l| l == lang) {
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
        "js" => "js",
        "csharp" => "cs",
        "md" => "md",
        // The bundled structural scanner's per-format streams each get a
        // distinct short prefix (one spawn per stream, each restarting its
        // opaque-id counter at `n1`), so merged structural streams cannot
        // collide opaque ids.
        "sh" => "sh",
        "yaml" => "ya",
        "json" => "jn",
        "toml" => "tm",
        "xml" => "xm",
        "dockerfile" => "df",
        "makefile" => "mk",
        "ini" => "ini",
        "misc" => "mi",
        "py" => "py",
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

/// The **target-set handoff protocol** (phase-02 task-9, PINNED INTERFACE —
/// feedback-85), the ONE contract every language frontend consumes.
///
/// CHANNEL: argv flags appended to each language's frontend command at the
/// spawn site, beside the existing `--module`/`--id-prefix`. `stdin` stays
/// `Stdio::null` (not the channel) and env vars are not the channel.
///
/// * `--targets <file>`: the absolute path to a UTF-8, newline-delimited file
///   of absolute source-file paths, one per line, no header, blanks ignored. An
///   absent flag or an empty file means "no emission filter". A language whose
///   target set is unchanged and empty is skipped entirely (phase-03 task-5).
/// * `--cache-dir <abs dir>`: the shared content-addressed store root
///   (`<git-common-dir>/apg/facts`).
/// * `--cache-key <key>`: the global cache key (`domain.value.cache-key`),
///   computed once by APG and passed unchanged to every frontend.
///
/// The frontend persists its native incremental artifact under
/// `<cache-dir>/<lang>/<cache-key>/`. Every frontend resolves against the FULL
/// context and only emission is filtered (`global.constraint.frontend-full-context`).
#[derive(Debug, Clone, Default)]
pub struct FrontendHandoff {
    /// When true, a `--targets <file>` list is written into the scan's temp dir
    /// and passed. `false` = no emission filter.
    pub targets_enabled: bool,
    /// The shared store root (`<git-common-dir>/apg/facts`).
    pub cache_dir: Option<PathBuf>,
    /// The global cache key token.
    pub cache_key: Option<String>,
}

impl FrontendHandoff {
    /// Writes the per-language target file for `lang` from the absolute target
    /// paths of that language's emission granularity, returning the path. A
    /// language with no targets writes an EMPTY file (the frontend treats
    /// "absent flag or empty file" as no filter; the spawn skip is phase-03
    /// task-5, not this filter).
    pub fn write_targets(&self, tmp: &Path, lang: &str, targets: &[String]) -> PathBuf {
        let path = tmp.join(format!("{lang}.targets"));
        let mut body = String::new();
        for t in targets {
            body.push_str(t);
            body.push('\n');
        }
        std::fs::write(&path, body).expect("write target list");
        path
    }

    /// Appends the handoff flags to a frontend `Command` for `lang`.
    pub fn append(&self, child: &mut Command, tmp: &Path, lang: &str, targets: &[String]) {
        if self.targets_enabled {
            let path = self.write_targets(tmp, lang, targets);
            child.arg("--targets").arg(path);
        }
        if let Some(dir) = &self.cache_dir {
            child.arg("--cache-dir").arg(dir);
        }
        if let Some(key) = &self.cache_key {
            child.arg("--cache-key").arg(key);
        }
    }
}

/// The `incremental::language_of` bucket a **scan language id** belongs to.///
/// The unified JS/TS frontend runs under TWO scan ids (`ts` and `js`) for ONE
/// artifact (`frontend_cmd` maps both to the same `tsfrontend`; the detector
/// returns `js` for a JS-only repo and `ts` for a TS-containing one, never
/// both), while `incremental::language_of` renders a single `"ts"` token for
/// every `.ts/.tsx/.mts/.cts/.js/.jsx/.mjs/.cjs` file. A JS-only repo's `"js"`
/// id must therefore compare against the `"ts"` bucket to receive its
/// `.js`-family targets; every other id is its own bucket. `language_of`
/// itself stays stable — it cannot know which of the two ids the repo is
/// scanning under.
fn language_bucket(scan_language: &str) -> &str {
    match scan_language {
        "js" => "ts",
        other => other,
    }
}

/// The absolute target paths belonging to `language`'s emission granularity,
/// from the checkout-relative target set.
pub fn targets_for_language(
    targets_rel: &std::collections::BTreeSet<String>,
    scan_root: &Path,
    language: &str,
) -> Vec<String> {
    let bucket = language_bucket(language);
    let mut out: Vec<String> = targets_rel
        .iter()
        .filter(|rel| incremental::language_of(rel) == bucket)
        .map(|rel| scan_root.join(rel).to_string_lossy().into_owned())
        .collect();
    out.sort();
    out
}

/// The warm-cache seed preparation (phase-06 tasks 3/4/6): when the shared store
/// holds a COMPLETE recorded scan for exactly this checkout's HEAD under the
/// current cache key, build the [`incremental::Prepared`] state from the
/// **recorded manifest alone** — never `Manifest::build`, so no full-tree walk —
/// and return it. `None` means the caller runs the ordinary
/// `incremental::prepare`.
///
/// The verdict comes from the pure `delta::scan_verdict` wrapper
/// ([`delta::scan_state`]): the recorded scan is at exactly HEAD under the
/// current key. Completeness is then judged without a source walk: every
/// source-language entry of the recorded manifest must have a stored fact unit
/// under the current key (non-source entries never get units and are ignored),
/// and every such language must have stored module scaffolding. A single missing
/// unit/scaffold is a miss (the caller falls back), so the warm path can never
/// assemble an incomplete graph. The recorded content-identity key must equal
/// this checkout's own, so a record from a different tree at the same sha is
/// refused.
fn warm_prepared(
    project_dir: &Path,
    apg_root: &Path,
    git_state: &git::GitState,
    scan_config: &cache::ScanConfigKey,
) -> Option<incremental::Prepared> {
    let cache_key = cache::CacheKey::compute(scan_config);
    let store_root = cache::FactStore::resolve(apg_root).ok()?.root;
    let (record, verdict) = delta::scan_state(apg_root, Some(&store_root), &cache_key);
    if !verdict.is_warm_cache() {
        return None;
    }
    let record = record?;
    // The recorded tree content must equal THIS checkout's: a record from a
    // dirty tree (or another commit's tree at the same sha) is not this tree.
    let (Some(current_key), Some(recorded_key)) = (
        git_state.content_key.as_deref(),
        record.content_key.as_deref(),
    ) else {
        return None;
    };
    if current_key != recorded_key {
        return None;
    }
    let store = cache::FactStore::at(store_root.clone()).load();
    let mut languages: BTreeSet<String> = BTreeSet::new();
    let mut reuse_candidates: Vec<incremental::ReuseFile> = Vec::new();
    for (rel, oid) in &record.manifest.entries {
        let lang = incremental::language_of(rel);
        if lang == "other" {
            // Not a scanned source file: it never carries a fact unit.
            continue;
        }
        // A source file with no stored unit means the cache is not complete
        // for this tree — fall back (conservative, never a partial assembly).
        store.candidate(lang, rel, oid, &cache_key)?;
        languages.insert(lang.to_string());
        reuse_candidates.push(incremental::ReuseFile {
            abs: incremental::absolute(project_dir, rel),
            rel: rel.clone(),
            oid: oid.clone(),
            lang: lang.to_string(),
        });
    }
    if reuse_candidates.is_empty() {
        return None;
    }
    for lang in &languages {
        // A language with no stored scaffolding cannot be reconstructed from
        // the cache — fall back.
        store.scaffolding(lang, &cache_key)?;
    }
    // The recorded manifest describes this tree; re-base its recorded root onto
    // the reading checkout so the re-recorded baseline stays coherent.
    let mut manifest = record.manifest.clone();
    manifest.root = project_dir.to_string_lossy().into_owned();
    Some(incremental::Prepared {
        store_root,
        cache_key,
        full_scan: None,
        targets_rel: BTreeSet::new(),
        changed_rel: BTreeSet::new(),
        removed_fqns: BTreeSet::new(),
        reuse_candidates,
        manifest,
        recorded_content_key: record.content_key.clone(),
    })
}

/// The code-FQN universe of the shared store's complete warm cache — the
/// full-universe seam's source when no local export exists yet (a fresh
/// worktree): every fragment's re-based code FQNs (module FQNs included) plus
/// every language's module scaffolding. The warm assembly produces exactly these
/// nodes, so authored `implemented-by` validation sees a full rebuild's universe
/// without the local `graph.jsonl` a full scan would have written.
fn warm_universe(
    store_root: &Path,
    cache_key: &cache::CacheKey,
    manifest: &cache::Manifest,
    languages: &BTreeSet<String>,
) -> BTreeSet<String> {
    let store = cache::FactStore::at(store_root.to_path_buf()).load();
    let mut out: BTreeSet<String> = BTreeSet::new();
    for (rel, oid) in &manifest.entries {
        let lang = incremental::language_of(rel);
        if lang == "other" {
            continue;
        }
        let Some((frag, stored_root)) = store.candidate(lang, rel, oid, cache_key) else {
            continue;
        };
        let (modules, nodes, _) = frag.project(&stored_root, "");
        out.extend(modules);
        for (fqn, node) in nodes {
            if node.status.is_none()
                && matches!(
                    node.kind,
                    graph::NodeKind::Module
                        | graph::NodeKind::Struct
                        | graph::NodeKind::Function
                        | graph::NodeKind::File
                )
            {
                out.insert(fqn);
            }
        }
    }
    for lang in languages {
        if let Some(scaffolding) = store.scaffolding(lang, cache_key) {
            out.extend(scaffolding.modules);
        }
    }
    out
}

/// The code-FQN universe contributed by this scan's **reused** fact units: each
/// reused file's re-based fragment FQNs plus every skipped language's replayed
/// module scaffolding — exactly the code nodes the win-B assembly splices in
/// from the shared store (the incremental sibling of [`warm_universe`]).
///
/// The incremental full-universe seam ([`incremental::full_universe`]) derives
/// its base from THIS checkout's previous export, which can lag the shared store
/// when another worktree scanned ahead: a file that is byte-identical to the
/// shared record but newer than the local export is reused (not re-emitted), so
/// its code FQNs would drop out of the universe and falsely trip `spec drift`
/// during `implemented-by` validation. Unioning this set in closes that gap.
fn reuse_universe(
    store_root: &Path,
    cache_key: &cache::CacheKey,
    files: &[(String, String, String)],
    scaffold_langs: &BTreeSet<String>,
) -> BTreeSet<String> {
    let store = cache::FactStore::at(store_root.to_path_buf()).load();
    let mut out: BTreeSet<String> = BTreeSet::new();
    for (rel, lang, oid) in files {
        let Some((frag, stored_root)) = store.candidate(lang, rel, oid, cache_key) else {
            continue;
        };
        let (modules, nodes, _) = frag.project(&stored_root, "");
        out.extend(modules);
        for (fqn, node) in nodes {
            if node.status.is_none()
                && matches!(
                    node.kind,
                    graph::NodeKind::Module
                        | graph::NodeKind::Struct
                        | graph::NodeKind::Function
                        | graph::NodeKind::File
                )
            {
                out.insert(fqn);
            }
        }
    }
    for lang in scaffold_langs {
        if let Some(scaffolding) = store.scaffolding(lang, cache_key) {
            out.extend(scaffolding.modules);
        }
    }
    out
}

/// The win-C per-language frontend spawn verdict (phase-03 task-5).
///
/// On the win-B incremental path (`full_scan == false`):
///
/// * a language with a **non-empty** target set spawns — it has changed files
///   to re-emit;
/// * a language with an **empty** target set is skipped **entirely** — not
///   merely emission-filtered (phase-02 task-8/-9, where an empty target set
///   still spawns) — and its per-file facts arrive through the win-B cached-fact
///   reuse path (`reuse_plan` below carries every file outside the target set).
///
/// The skip applies only to the **PARTIAL** case the task names: the scan as a
/// whole has a non-empty target set and *this* language is one of the unchanged
/// ones. When the whole target set is empty, phase-02 deliberately runs every
/// frontend **UNFILTERED** — an empty/absent `--targets` list means "no filter"
/// (`incremental.rs` "an EMPTY target set emits every fact (no filter), so the
/// full spool is already the universe and reuse stays available"). That is not
/// merely an optimisation: the frontends emit their **global module
/// scaffolding** (e.g. Go's `Module` records and the `Module -> Module` package
/// hierarchy, `golib/main.go:256-268`) *outside* the per-file emission filter,
/// because those records carry no location and so are not part of any per-file
/// fact unit. The cached-fact reuse path can only rebuild modules it can see as
/// a File's parent, so skipping when nothing changed would drop, say, Go's
/// `scratch/a` package module and make the incremental graph differ from a full
/// scan (the cross-worktree reuse acceptance). The whole-tree FRESH fast-path
/// (phase-01 task-7) is the separate, earlier path for "the entire tree is
/// unchanged".
///
/// **Warm-cache completeness (phase-06 task-1).** When the shared store holds a
/// COMPLETE recorded scan for exactly this HEAD (`warm_complete`), every
/// language — including its global module scaffolding (feedback-102) — is
/// reconstructible from the store, so NO language spawns on the empty-target
/// case the paragraph above reserves for the unfiltered path. The flag is
/// derived from the same recorded manifest + store completeness that drives the
/// warm-cache assembly (`cmd_scan`), never re-derived here.
///
/// On a whole-tree full scan (`full_scan == true`) every detected/requested
/// language must still spawn; the skip applies only to the incremental partition
/// of work.
///
/// This is the **single source of truth** for the per-language verdict: it is
/// derived from the very phase-2 target set that drives the win-C DB splice
/// (`PipelineInput { targets_rel, .. }`, phase-03 task-4), never from a fresh
/// `auto_detect_languages` walk, so a language skipped here is exactly a
/// language the splicer treats as unchanged and the two can never disagree.
fn should_spawn_language(
    full_scan: bool,
    targets_empty: bool,
    any_targets: bool,
    warm_complete: bool,
) -> bool {
    if full_scan {
        return true;
    }
    if warm_complete {
        // The recorded-HEAD scan is complete in the shared store: assemble from
        // its rebased facts (per-file units + replayed scaffolding) with zero
        // frontends, even though the delta target set is empty.
        return false;
    }
    !any_targets || !targets_empty
}

/// The scan-time tools a selected language's frontend requires on `PATH` when
/// `apg scan` runs it (SPEC: `solution.component.scan-time-preflight`). A pure
/// mapping — no I/O. The self-contained frontends (`cpp`, `csharp`, `py`) and
/// the bundled structural scanner's every stream id (`md`, `sh`, `yaml`,
/// `json`, `toml`, `xml`, `dockerfile`, `makefile`, `ini`, `misc`) require
/// nothing — the structural arm is explicit so the exemption is intentional,
/// not the unknown-id catch-all — and neither does an unknown language id
/// (never a false refusal). `ts`/`js` share the one unified frontend and its
/// `node` runtime.
fn scan_time_tools(language: &str) -> &'static [&'static str] {
    match language {
        "go" => &["go"],
        "java" => &["java"],
        "rust" => &["cargo", "rustc"],
        "ts" | "js" => &["node"],
        // The bundled structural scanner is self-contained: it never shells out
        // and needs nothing on `PATH`, so every structural stream id is
        // tool-less.
        "md" | "sh" | "yaml" | "json" | "toml" | "xml" | "dockerfile" | "makefile" | "ini"
        | "misc" => &[],
        _ => &[],
    }
}

/// The named, actionable error for a scan-time tool the selected language's
/// frontend needs but `PATH` does not provide: the language, the missing tool,
/// and how to install it. A pure builder — no panic, no I/O (SPEC:
/// `global.constraint.scan-time-tool-failure-actionable`).
fn scan_time_tool_error(language: &str, tool: &str) -> String {
    let hint = match language {
        "go" => "install Go with `brew install go`",
        "java" => "install a JDK >= 21 (e.g. `brew install openjdk@21`)",
        "rust" => "install the Rust toolchain with `brew install rust`",
        "ts" | "js" => "install Node.js with `brew install node`",
        _ => "install the required toolchain",
    };
    format!(
        "scan-time toolchain preflight failed for the `{language}` frontend: \
         required tool `{tool}` was not found on PATH — {hint}, then re-run `apg scan`"
    )
}

/// Probes `PATH` for every tool `language`'s frontend needs and returns
/// [`scan_time_tool_error`] for the first miss. This is the I/O unit (a real
/// `PATH` probe); a tool-less language probes nothing and succeeds — the
/// self-contained `cpp`/`csharp`/`py` frontends and every structural stream id
/// of the bundled scanner (`md`, `sh`, `yaml`, `json`, `toml`, `xml`,
/// `dockerfile`, `makefile`, `ini`, `misc`) are all tool-less.
fn scan_time_preflight(language: &str) -> anyhow::Result<()> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    for &tool in scan_time_tools(language) {
        let found = std::env::split_paths(&path)
            .any(|dir| dir.join(tool).is_file() || dir.join(format!("{tool}.exe")).is_file());
        if !found {
            anyhow::bail!("{}", scan_time_tool_error(language, tool));
        }
    }
    Ok(())
}

/// Spawns one language's frontend for a scan phase, draining stdout to a spool
/// and stderr to a log spool. Returns `Ok(Some(spool))` on success and
/// `Ok(None)` when the frontend process itself failed (reported + skipped,
/// never fatal). An `Err` is reserved for a pre-spawn toolchain failure (a
/// missing scan-time tool), which fails the whole scan with a named, actionable
/// error rather than silently skipping the language and writing an empty graph.
#[allow(clippy::too_many_arguments)]
fn spawn_frontend(
    lang: &str,
    project_dir: &Path,
    module_dirs: &[String],
    path_excludes: &[String],
    no_build_scripts: bool,
    multi: bool,
    handoff: &FrontendHandoff,
    tmp: &Path,
    targets: &[String],
    phase: u32,
    log: &mut Log,
) -> anyhow::Result<Option<PathBuf>> {
    // Runtime scan-time toolchain preflight (SPEC:
    // `solution.component.scan-time-preflight`): a selected language whose
    // required tool is absent fails the whole scan here, before any spawn, with
    // the named actionable error — never the cryptic panic this replaces, never
    // a silent skip, never an empty graph. The tool-less frontends
    // (`cpp`, `csharp`, `py`) and every structural stream id of the bundled
    // scanner (`md`, `sh`, …) probe nothing and are unaffected.
    scan_time_preflight(lang)?;
    let cmd = frontend_cmd(lang)
        .ok_or_else(|| anyhow::anyhow!("frontend for language '{lang}' is not installed"))?;
    let spool = tmp.join(format!("{lang}.p{phase}.jsonl"));
    let spool_file = std::fs::File::create(&spool).unwrap();
    let stderr_spool = tmp.join(format!("{lang}.p{phase}.stderr"));
    let stderr_file = std::fs::File::create(&stderr_spool).unwrap();
    // `cmd` is a full command line (e.g. "node /path/scanner.mjs" or the java
    // wrapper); split it into argv so every frontend spawns the same way.
    let mut parts = cmd.split_whitespace();
    let prog = parts.next().expect("empty frontend command");
    let mut child = Command::new(prog);
    child.args(parts).arg(project_dir.display().to_string());
    for m in module_dirs {
        child.arg("--module").arg(m);
    }
    if lang == "rust" && no_build_scripts {
        child.arg("--no-build-scripts");
    }
    if multi {
        child.arg("--id-prefix").arg(id_prefix_for(lang));
    }
    // The per-stream selector for the bundled structural scanner: ONE binary
    // serves every structural stream id, so the spawn must name the stream it
    // emits. `spawn_frontend` is the SINGLE OWNER of `--stream` (frontend_cmd
    // returns only the command line); it is appended unconditionally for a
    // structural id — even a single-language scan must not re-emit the whole
    // structural graph under one `lang_switch`.
    if classify::is_structural_language(lang) {
        child.arg("--stream").arg(lang);
    }
    // Win-B target-set hand-off (task-9) plus the pinned cache hand-off on the
    // full-scan path (phase-04 tasks 33/34): append whenever ANY part of the
    // hand-off is present. `--targets` is still emitted only when
    // `targets_enabled`, while `--cache-dir`/`--cache-key` are appended whenever
    // set — so a full scan (`targets_enabled == false`) passes the cache flags
    // and NO `--targets`, and the incremental argv is unchanged in flags AND
    // order (AC (b)). With no hand-off at all (no store / `NotAGitRepo`) nothing
    // is appended (AC (c)).
    if handoff.targets_enabled || handoff.cache_dir.is_some() || handoff.cache_key.is_some() {
        handoff.append(&mut child, tmp, lang, targets);
    }
    child
        .args(path_excludes)
        .stdin(Stdio::null())
        .stdout(Stdio::from(spool_file.try_clone().unwrap()))
        .stderr(Stdio::from(stderr_file.try_clone().unwrap()));
    log.ln(&format!("[scan] running {lang} frontend..."));
    let mut frontend_output = child
        .spawn()
        .map_err(|e| anyhow::anyhow!("failed to run the {lang} frontend (`{cmd}`): {e}"))?;
    let ok = frontend_output
        .wait()
        .map_err(|e| anyhow::anyhow!("couldn't wait for the {lang} frontend: {e}"))?
        .success();
    log.append_file(&stderr_spool);
    if !ok {
        log.ln(&format!(
            "[scan] {lang} frontend failed; skipping this language"
        ));
        for line in tail_of(&stderr_spool, 10) {
            log.ln(&format!("  [{lang}] {line}"));
        }
        return Ok(None);
    }
    log.ln(&format!("[scan] {lang} frontend exited"));
    Ok(Some(spool))
}

/// Parses the `--targets <file>` list: newline-delimited absolute paths, one
/// per line, blanks ignored. The frontend consumes the file directly (the
/// contract is the file format); this helper exists so the integration test can
/// assert the exact format.
pub fn read_targets_file(path: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

/// The `apg --help` text. Split from [`print_help`] so the strict add|update|rm
/// surface and the unchanged `apg review` line can be asserted directly by the
/// Phase-7 help/dispatch acceptance test (no stdout capture).
pub fn help_text() -> String {
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
                               csharp, py (comma-separated or repeated;
                               auto-detected for every language present if
                               omitted)
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
pub fn main() {
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
pub fn duplicate_install_files(project_opencode: &Path, user_opencode: &Path) -> Vec<PathBuf> {
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
pub fn prune_stale_suite(opencode_dir: &Path) -> std::io::Result<usize> {
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
pub fn install_suite(opencode_dir: &Path) -> anyhow::Result<(usize, usize)> {
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
/// project `.opencode/` by the agent-builder, not by init; that template
/// delivers the coordinator-mediated writer/reviewer grant shape — writers
/// hold the read-only `apg_review` channel and return a claim, and the
/// coordinator performs the shallow claim-vs-change check and then actions the
/// item — so a scaffolded reviewer never inherits writer-action wording. Init
/// is the layout's versioning/upgrade act (R9/R10): it re-runs idempotently
/// and writes the binary version into `apg/config.json` (user code_type rules
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
pub fn scaffold_gitignore(dir: &Path) -> anyhow::Result<bool> {
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
pub fn emit_json_rows(result: lbug::QueryResult<'_>) -> String {
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
        content_key: git_state.content_key.clone(),
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
    // Per-phase timing (phase-04): `scan_start` is taken before any work so the
    // startup/overhead phase spans argument parsing onward.
    let scan_start = std::time::Instant::now();
    let mut timing = timing::TimingReport::new();
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

    // The checkout-independent identity base (`requirements.requirement.
    // repo-relative-file-identity`): the git toplevel, or the scan root when
    // the tree is not a git repository. Every scanner path is rendered
    // repo-relative against it, so `apg scan <repo>` and `apg scan
    // <repo>/subdir` mint the same File identity and two worktrees agree.
    let identity_base = git::repo_rel(&project_dir);

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

    // Win-A fast path (scan-freshness): when the pre-scan DB's recorded content
    // identity matches the tree exactly, the existing DB is reusable and every
    // language frontend is skipped — reuse the DB and return BEFORE any
    // frontend spawn or DB rebuild. The predicate is content identity (never
    // mtime) and is the same rule `is_stale`/`staleness_line` use, so the
    // printed verdict and the fast-path decision can never disagree. A stale
    // tree falls through to the full pipeline below.
    if git::is_fresh(&apg_root) {
        log.ln(
            "[scan] fast-path: tree unchanged since the recorded scan — reusing db.lbug (frontends skipped)",
        );
        // Phase-04 task-3: the whole-tree fast path runs no frontend and
        // rebuilds nothing, so all the elapsed time is startup/overhead and the
        // frontend phase is reported `frontend-skipped` (with ingest-assembly
        // and db-load at zero, every phase key still present).
        timing.record(timing::Phase::Startup, scan_start.elapsed());
        timing.mark_frontend_skipped();
        emit_timing(&mut log, &timing);
        return Ok(());
    }

    let available = available_languages();
    if available.is_empty() {
        panic!(
            "No scanner frontends found. Install one via brew (e.g. `brew install antz29/apg/apg-go`), the curl installer (e.g. `install.sh go`), set APG_FRONTEND_DIR, or rebuild with the required toolchain."
        );
    }

    let mut languages: Vec<String> = if !language_args.is_empty() {
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
    // The structural stream ids are selected whenever the bundled
    // `structfrontend` is installed: they are not extension-detectable (the
    // driver enumerates them), so neither `--language` nor auto-detection can
    // name them, and the selection must not drop them. Appended in canonical
    // order after the code languages.
    for lang in STRUCTURAL_LANGUAGES {
        if available.iter().any(|l| l == lang) && !languages.iter().any(|l| l == lang) {
            languages.push(lang.to_string());
        }
    }
    log.ln(&format!("Languages: {}", languages.join(", ")));

    if !blacklist.is_empty() {
        log.ln(&format!("Blacklist: {:?}", blacklist));
    }
    if !path_excludes.is_empty() {
        log.ln(&format!("Path excludes: {:?}", path_excludes));
    }

    let config = classify::ApgConfig::load(&project_dir);

    // The scan config identity shared by the warm-cache probe and the win-B
    // preparation: the exact languages/excludes/modules this scan runs with.
    let scan_config = cache::ScanConfigKey {
        languages: languages.clone(),
        excludes: path_excludes.clone(),
        modules: module_dirs.clone(),
    };

    // Win-B incremental preparation (phase-02 task-8): the content manifest,
    // git delta + correctness fallbacks, impact target set, and the fact-reuse
    // candidates. `full_scan: Some(reason)` falls through to the full pipeline
    // (the correctness reference). The target set drives the frontend
    // `--targets` hand-off (task-9) and the fact splice in `run_pipeline`.
    //
    // Warm-cache seed (phase-06 task-4): when the shared store already holds a
    // COMPLETE recorded scan for exactly this HEAD, prepare from the recorded
    // manifest and assemble from its re-based facts — no `Manifest::build`, so
    // no full-tree walk, and zero frontends. The ordinary `prepare` runs only
    // when the warm probe misses. A blacklist is never folded into the cache
    // key, so a blacklisted scan falls back to the ordinary path (never a warm
    // reuse of facts recorded under a different FQN filter).
    let warm = if blacklist.is_empty() {
        warm_prepared(&project_dir, &apg_root, &git_state, &scan_config)
    } else {
        None
    };
    let warm_complete = warm.is_some();
    let incremental =
        warm.unwrap_or_else(|| incremental::prepare(&project_dir, &apg_root, &scan_config));
    let mut handoff = FrontendHandoff::default();
    let mut reuse_plan: Option<incremental::ReusePlan> = None;
    if let Some(reason) = &incremental.full_scan {
        log.ln(&format!("[scan] {}", reason.describe()));
        // Phase-04 task-33: carry the pinned cache hand-off on the FULL-scan
        // path too, so each frontend's full-scan native-artifact seeding (the
        // Java class surface behind tasks 17/32) is reachable through the CLI.
        // `targets_enabled` stays FALSE — a full scan emits everything and must
        // carry NO `--targets` (phase-02 task-9). The no-store case
        // (`FullScanReason::NotAGitRepo` leaves `store_root` empty) yields
        // `None`/`None`: never a fabricated or defaulted cache path (AC (c)).
        if !incremental.store_root.as_os_str().is_empty() {
            handoff.cache_dir = Some(incremental.store_root.clone());
            handoff.cache_key = Some(incremental.cache_key.token());
        }
    } else if warm_complete {
        log.ln(&format!(
            "[scan] warm cache: recorded scan at HEAD — assembling {} file(s) from re-based facts (frontends skipped)",
            incremental.reuse_candidates.len(),
        ));
        // No `--targets`: nothing is re-emitted; the per-file units are spliced
        // from the store and their languages' scaffolding is replayed from the
        // store's `skipped_langs` set.
        handoff.cache_dir = Some(incremental.store_root.clone());
        handoff.cache_key = Some(incremental.cache_key.token());
        reuse_plan = Some(incremental::ReusePlan {
            store_root: incremental.store_root.clone(),
            cache_key: incremental.cache_key.clone(),
            files: incremental
                .reuse_candidates
                .iter()
                .map(|f| (f.rel.clone(), f.lang.clone(), f.oid.clone()))
                .collect(),
            reader_root: project_dir.to_string_lossy().into_owned(),
            // Every language is reconstructible from the store; the final set is
            // refilled after the (zero-spawn) loop from the same full cache.
            skipped_langs: languages.iter().cloned().collect(),
        });
    } else {
        log.ln(&format!(
            "[scan] incremental: {} target file(s), {} reusable file(s)",
            incremental.targets_rel.len(),
            incremental.reuse_candidates.len(),
        ));
        // The pinned target-set hand-off contract (task-9): `--targets`,
        // `--cache-dir`, `--cache-key` on every language's argv. The target
        // files live in the scan's own temp dir (removed with it).
        handoff.targets_enabled = true;
        handoff.cache_dir = Some(incremental.store_root.clone());
        handoff.cache_key = Some(incremental.cache_key.token());
        reuse_plan = Some(incremental::ReusePlan {
            store_root: incremental.store_root.clone(),
            cache_key: incremental.cache_key.clone(),
            files: incremental
                .reuse_candidates
                .iter()
                .map(|f| (f.rel.clone(), f.lang.clone(), f.oid.clone()))
                .collect(),
            reader_root: project_dir.to_string_lossy().into_owned(),
            // Filled from the FINAL target set after the spawn loop.
            skipped_langs: BTreeSet::new(),
        });
    }

    // Each frontend's stderr (progress + compiler diagnostics) is spooled to a
    // per-language temp file, then folded into the log file. On a non-zero
    // exit the tail is also reported to the terminal (SPEC 0.9.1 R1).
    log.ln("Frontend progress -> apg-frontend.log");

    // Startup/overhead ends where the frontend work begins (phase-04 task-2):
    // argument parsing, git state, the version gate, layout discovery, and the
    // win-B incremental preparation all fall in this phase.
    timing.record(timing::Phase::Startup, scan_start.elapsed());
    let frontend_start = std::time::Instant::now();

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

    // Phase 1: the changed files ∪ overload peers (the stage-1 target set).
    // The signature early-cutoff (phase-02 task-6) is applied AFTER this pass:
    // the phase-1 stream yields the changed files' new exported signatures, and
    // only a genuine signature change pulls the reverse-dependency closure into
    // phase 2. A body-only change ends after phase 1, so its dependents are
    // reused.
    let full_scan_path = incremental.full_scan.is_some();
    let mut phase = 1u32;
    let mut targets_rel = incremental.targets_rel.clone();
    loop {
        // The scan as a whole has work to re-emit (the PARTIAL case). When it
        // does not, phase-02 runs every frontend unfiltered instead of skipping
        // (see `should_spawn_language`).
        let any_targets = !targets_rel.is_empty();
        for lang in &languages {
            let targets = targets_for_language(&targets_rel, &project_dir, lang);
            // Win-C per-language spawn skip (phase-03 task-5): on the
            // incremental path an unchanged language (empty target set while
            // the scan has other changed languages) has its frontend process
            // skipped entirely; its per-file facts arrive via the win-B
            // cached-fact reuse path. On a full scan every language spawns. A
            // phase-2 language whose only targets are its phase-1 ones still
            // has them in the final (stage-1 ∪ cascade) set, so it re-runs and
            // replaces its spool; only a language with no targets at all is
            // skipped here.
            if !should_spawn_language(
                full_scan_path,
                targets.is_empty(),
                any_targets,
                warm_complete,
            ) {
                if phase == 1 {
                    if warm_complete {
                        log.ln(&format!(
                            "[scan] {lang}: recorded scan at HEAD — frontend skipped (warm cache)"
                        ));
                    } else {
                        log.ln(&format!(
                            "[scan] {lang}: no changed targets — frontend skipped (facts reused)"
                        ));
                    }
                }
                continue;
            }
            match spawn_frontend(
                lang,
                &project_dir,
                &module_dirs,
                &path_excludes,
                no_build_scripts,
                multi,
                &handoff,
                &tmp,
                &targets,
                phase,
                &mut log,
            )? {
                Some(spool) => {
                    // A phase-2 re-run replaces the language's phase-1 spool, so
                    // each language contributes exactly one stream (no duplicate
                    // emission / FQN collision).
                    spools.retain(|(l, _)| l != lang);
                    spools.push((lang.clone(), spool));
                }
                None => failed.push(lang.clone()),
            }
        }
        if phase == 2 || incremental.full_scan.is_some() {
            break;
        }
        // Apply the signature early-cutoff using the phase-1 stream.
        let extra = {
            let phase1_lang = if languages.len() == 1 {
                languages[0].clone()
            } else {
                languages.join(",")
            };
            let (phase1_graph, _) = ingest::ingest(
                scanner_records(&spools[..], &git_state),
                &ingest::IngestOptions {
                    blacklist: &blacklist,
                    language: &phase1_lang,
                    config: config.as_ref(),
                    base: Some(&identity_base),
                },
            );
            incremental::extra_cascade_targets(
                &incremental.store_root,
                &project_dir,
                &phase1_graph,
                &targets_rel,
            )
        };
        if extra.is_empty() {
            break;
        }
        log.ln(&format!(
            "[scan] signature change cascades to {} dependent file(s)",
            extra.len()
        ));
        targets_rel.extend(extra);
        // Phase 2 re-spawns the languages that gained targets over the UNION
        // (stage-1 ∪ cascade), with the spools accumulated above: a language
        // that gained targets is dropped and re-spawned so its stream covers
        // the union exactly once.
        phase = 2;
    }
    // The frontend phase covers the whole spawn loop — both the stage-1 pass
    // and any signature-cascade stage-2 re-runs (phase-04 task-2).
    timing.record(timing::Phase::Frontend, frontend_start.elapsed());
    // The final re-emission target set (stage 1 ∪ the signature cascade).
    let targets_rel = targets_rel;
    // Rebuild the reuse plan against the FULL target set, so a cascaded
    // dependent is re-emitted rather than reused from stale facts.
    if incremental.full_scan.is_none() {
        let files: Vec<(String, String, String)> = incremental
            .reuse_candidates
            .iter()
            .filter(|f| !targets_rel.contains(&f.rel))
            .map(|f| (f.rel.clone(), f.lang.clone(), f.oid.clone()))
            .collect();
        // The languages whose frontend was skipped this scan: every language
        // with reused facts that has NO file in the final target set. Derived
        // from the SAME final target set that drives the spawn skip and the DB
        // splice, so the scaffolding replay and the spawn skip can never
        // disagree; a language with any target is spawned and emits its own
        // (fresh) scaffolding.
        //
        // Warm-cache completeness (phase-06 task-1/4): every configured language
        // was skipped, so EVERY language's scaffolding must be replayed from the
        // store — the empty-target-set case below.
        let skipped_langs: BTreeSet<String> = if warm_complete {
            languages.iter().cloned().collect()
        } else if targets_rel.is_empty() {
            BTreeSet::new()
        } else {
            let with_targets: BTreeSet<&str> = targets_rel
                .iter()
                .map(|rel| incremental::language_of(rel))
                .collect();
            files
                .iter()
                .map(|(_, lang, _)| lang)
                .filter(|lang| !with_targets.contains(lang.as_str()))
                .cloned()
                .collect()
        };
        reuse_plan = Some(incremental::ReusePlan {
            store_root: incremental.store_root.clone(),
            cache_key: incremental.cache_key.clone(),
            files,
            reader_root: project_dir.to_string_lossy().into_owned(),
            skipped_langs,
        });
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
    // Ingest-assembly (phase-04 task-4) starts at the pre-ingest/universe work
    // below and continues through `run_pipeline`'s own ingestion; the DB build
    // after that in `run_pipeline` is the db-load phase.
    let assembly_start = std::time::Instant::now();

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
    //
    // FULL-UNIVERSE SEAM (feedback-92): on the win-B incremental path the spool
    // holds only the re-emitted target facts, so a language skipped/emission-
    // filtered this scan would vanish from the universe and `validate_code_refs`
    // would falsely bail `spec drift`. Derive the FULL universe instead from the
    // PREVIOUS export (the sole full code-identity source) MINUS the delta's
    // removed FQNs UNION the delta's emitted real code FQNs — never the
    // target-only spool. On a full scan the full spool IS the universe.
    //
    // Warm-cache path (phase-06 task-4): the spool is EMPTY (zero frontends) and
    // a fresh worktree has no local export, so derive the universe from the
    // shared store's re-based facts + module scaffolding — exactly the nodes the
    // warm assembly produces.
    let scanned_code: BTreeSet<String> = if warm_complete {
        let warm_langs: BTreeSet<String> = languages.iter().cloned().collect();
        warm_universe(
            &incremental.store_root,
            &incremental.cache_key,
            &incremental.manifest,
            &warm_langs,
        )
    } else if incremental.full_scan.is_some() {
        let (pre, _) = ingest::ingest(
            scanner_records(&spools, &git_state),
            &ingest::IngestOptions {
                blacklist: &blacklist,
                language: &cleanup_language,
                config: config.as_ref(),
                base: Some(&identity_base),
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
    } else {
        // The delta's emitted real code FQNs: pre-ingest the target-only spool
        // to discover exactly which FQNs this scan's frontends produced.
        let (emitted, _) = ingest::ingest(
            scanner_records(&spools, &git_state),
            &ingest::IngestOptions {
                blacklist: &blacklist,
                language: &cleanup_language,
                config: config.as_ref(),
                base: Some(&identity_base),
            },
        );
        let emitted_fqns: BTreeSet<String> = emitted
            .nodes
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
            .collect();
        let mut universe = incremental::full_universe(&apg_root, &incremental, &emitted_fqns);
        // The reused files' facts are spliced from the shared store, not
        // re-emitted, so they contribute no spool FQNs. This checkout's previous
        // export can lag the shared store (another worktree scanned ahead), so
        // union in the reused units' own code FQNs — otherwise a reused file
        // newer than the local export would drop out of the universe and falsely
        // trip `spec drift`.
        if let Some(plan) = &reuse_plan {
            universe.extend(reuse_universe(
                &plan.store_root,
                &plan.cache_key,
                &plan.files,
                &plan.skipped_langs,
            ));
        }
        universe
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

    // The win-B pipeline input: the store to splice cached facts from and to
    // record the completed scan back into. The store is ALWAYS recorded (a full
    // scan records the cold baseline the next scan diffs against); only `reuse`
    // is `None` on the full path.
    let pipeline_input = if incremental.store_root.as_os_str().is_empty() {
        None
    } else {
        Some(incremental::PipelineInput {
            store_root: Some(incremental.store_root.clone()),
            cache_key: incremental.cache_key.clone(),
            scan_root: project_dir.clone(),
            manifest: incremental.manifest.clone(),
            sha: git_state.sha.clone().unwrap_or_default(),
            reuse: reuse_plan.clone(),
            // The win-C splice's delete scope and subtraction set are the SAME
            // phase-2 state that drove the frontend target hand-off: the FINAL
            // target set (stage-1 ∪ the signature cascade) and the delta's
            // removed FQNs. Threaded, never re-derived (phase-03 task-4).
            targets_rel: targets_rel.clone(),
            removed_fqns: incremental.removed_fqns.clone(),
            // The shared recorded content identity the delta was derived from,
            // captured before the completed scan rewrote `scan.json` — the
            // splice's equivalence guard (feedback-101).
            recorded_content_key: incremental.recorded_content_key.clone(),
        })
    };

    timing.add(timing::Phase::IngestAssembly, assembly_start.elapsed());
    let pipeline_timings = run_pipeline(
        records,
        &blacklist,
        &path_excludes,
        &cleanup_language,
        config.as_ref(),
        pipeline_input.as_ref(),
        Some(&identity_base),
        &mut log,
    );
    timing.add(
        timing::Phase::IngestAssembly,
        pipeline_timings.ingest_assembly,
    );
    timing.record(timing::Phase::DbLoad, pipeline_timings.db_load);
    let _ = std::fs::remove_dir_all(&tmp);
    log.ln("[scan] spool temp dir removed");
    // The per-phase report is first-class scan output (phase-04 task-2): it is
    // emitted on every non-panicking path, including a partial graph after a
    // frontend failure.
    emit_timing(&mut log, &timing);
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
///
/// `input` carries the win-B incremental state when the scan is incremental
/// (phase-02 task-7/task-8): the cached fact units to splice into the assembly
/// and the store to record the completed scan back into. `None` on the full
/// path (and for the hermetic test harness), where assembly is spool-only.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_pipeline(
    records: impl IntoIterator<Item = schema::Record>,
    blacklist: &[String],
    path_excludes: &[String],
    language: &str,
    config: Option<&classify::ApgConfig>,
    input: Option<&incremental::PipelineInput>,
    base: Option<&Path>,
    log: &mut Log,
) -> timing::PipelineTimings {
    // Phase-04 task-4: this function owns two reported phases. Ingest-assembly
    // covers the ingestor passes, cleanup, and the fact-store recording from
    // entry to the DB dispatch below; db-load covers that dispatch (splice OR
    // parquet/Database::new/create_schema/copy_from) and the `graph.jsonl`
    // export.
    let assembly_start = std::time::Instant::now();
    let (mut graph, report) = {
        // Stream the scanner JSONL straight into the ingestor. On the win-B
        // path the target-only spool is ingested here and the unaffected files'
        // cached fact units are spliced into the SAME in-memory graph
        // (phase-02 task-7 owns this assembly on every path). The resulting
        // full fact-spliced graph is the single graph consumed downstream.
        let reuse_holder;
        let reuse = match input.and_then(|i| i.reuse.as_ref()) {
            Some(plan) => {
                reuse_holder = cache::FactStore::at(plan.store_root.clone()).load();
                Some(plan.reuse(&reuse_holder))
            }
            None => None,
        };
        if let Some(r) = reuse.as_ref() {
            ingest::ingest_with_reuse(
                records,
                &ingest::IngestOptions {
                    blacklist,
                    language,
                    config,
                    base,
                },
                Some(r),
            )
        } else {
            ingest::ingest(
                records,
                &ingest::IngestOptions {
                    blacklist,
                    language,
                    config,
                    base,
                },
            )
        }
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

    // Win-B: record the just-assembled graph back into the shared
    // content-addressed store (manifest, scan record, per-file fact units, the
    // portable dep/signature/overload indexes). Recording is a best-effort
    // cache write — a failure downgrades the next scan to a full scan, never
    // the current graph.
    if let Some(input) = input
        && let Some(store_root) = &input.store_root
    {
        match incremental::record(
            store_root,
            &input.cache_key,
            &input.scan_root,
            &graph,
            &input.manifest,
            &input.sha,
            // The SAME phase-2 re-emission target set that drove the frontend
            // hand-off and the win-C splice, threaded through — never re-derived.
            // Empty on the full-scan fallback, so `record` keeps its full-scan
            // behaviour (phase-04 task-28 / task-13 AC (b)).
            &input.targets_rel,
        ) {
            Ok(()) => log.ln("[scan] content-addressed facts recorded"),
            Err(e) => log.ln(&format!("[scan] fact recording skipped: {e:#}")),
        }
    }

    // Ingest-assembly ends here; the DB build dispatch below is the db-load
    // phase (phase-04 task-4).
    let ingest_assembly = assembly_start.elapsed();
    let db_start = std::time::Instant::now();

    // `run_pipeline` runs from inside `<apg_root>/.trans` (both `cmd_scan` and
    // the hermetic test harness chdir there), so the previous/next artifacts
    // are `<apg_root>/.trans/{db.lbug,graph.jsonl}` (SPEC §6).
    let apg_root = std::env::current_dir()
        .ok()
        .and_then(|cwd| cwd.parent().map(Path::to_path_buf));

    // Defense in depth (phase-03 lifecycle exclusivity): never unlink — or seed
    // from — a DB a live session holds. `cmd_scan` refuses earlier; this guard
    // catches a session that started mid-scan before the projected DB is
    // replaced.
    if let Some(apg_root) = &apg_root
        && session::live_session(apg_root)
    {
        panic!(
            "refused: a live apg session owns this db.lbug — run `apg session end` before scanning"
        );
    }

    // ---- DB build dispatch (win C, phase-03 task-4) ------------------------
    //
    // On the win-B incremental path the previous `db.lbug` already holds every
    // unaffected row, so seed a copy of it, apply the phase-2 delta as DML
    // (`splice`), and publish BOTH artifacts atomically instead of rebuilding
    // from scratch. The existing full load (remove + create_schema + copy_from
    // + write_graph_jsonl) stays the correctness reference and runs whenever the
    // splice is ineligible or fails mid-sequence. `input.reuse` is `Some`
    // exactly on the incremental path (it is `None` on every correctness
    // full-scan fallback), and `input.targets_rel`/`removed_fqns` are the SAME
    // phase-2 sets that drove the frontend target hand-off.
    let splice_report = match (input, apg_root.as_deref()) {
        (Some(input), Some(apg_root)) => try_splice_build(&graph, input, apg_root, log),
        _ => None,
    };
    if let Some(report) = splice_report {
        log.ln(&format!(
            "[load] splice: {} node(s) upserted, {} deleted, {} rel(s) re-inserted, {} unresolved GC'd, scan row refreshed: {}; full load skipped",
            report.nodes_upserted,
            report.nodes_deleted,
            report.edges_merged,
            report.unresolved_gc,
            report.scan_refreshed,
        ));
        // Export routing (phase-03 task-7): on the splice branch the export is
        // serialized by the UNCHANGED `load::write_graph_jsonl` into a
        // same-directory `.graph-*.tmp` sibling inside `splice::publish` above
        // and renamed in AFTER the spliced DB (DB first, then export), so both
        // artifacts of the P2-assembled full in-memory graph land atomically.
        // The full-load branch below keeps the standalone direct `graph.jsonl`
        // write, so every Export record kind/property still comes from the one
        // tested writer.
    } else {
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
    timing::PipelineTimings {
        ingest_assembly,
        db_load: db_start.elapsed(),
    }
}

/// The win-C DB-build dispatch (phase-03 task-4): try to seed the working
/// `db.lbug` from the previous scan and apply the phase-2 delta as DML, then
/// publish `db.lbug` + `graph.jsonl` atomically (`splice::publish`).
///
/// Returns the splice report on success; `None` on any ineligibility or
/// mid-sequence failure, in which case the caller runs the existing full load
/// (the correctness reference) and the previous artifacts are left in place.
/// This function never panics: a failure is a logged fallback.
///
/// `graph` is the win-B assembled graph (re-emitted target units PLUS cached
/// unaffected units) and `input` carries the same phase-2 target/removed sets
/// that drove the frontend hand-off — the delete scope is never re-derived.
///
/// The seed is EQUIVALENCE-GUARDED (feedback-101): the delta/manifest are
/// shared across worktrees while `db.lbug` is local, so the splice is refused
/// unless this worktree's seed was built from the same tree content the shared
/// [`incremental::Prepared::recorded_content_key`] names — see
/// [`splice::seed_checked`]. A refusal is a full load (the correctness
/// reference), never a published DB that diverges from a rebuild.
pub fn try_splice_build(
    graph: &graph::Graph,
    input: &incremental::PipelineInput,
    apg_root: &Path,
    log: &mut Log,
) -> Option<splice::SpliceReport> {
    // Eligibility: the win-B incremental path (a phase-2 delta/manifest exists)
    // with a previous DB to seed from. A full-scan fallback must never splice —
    // it has no target/removed set and its graph is the full universe.
    input.reuse.as_ref()?;
    let db = splice::db_path(apg_root);
    if !db.exists() {
        log.ln("[load] splice: no previous db.lbug to seed from — full load");
        return None;
    }
    // The seed is a WHOLE-FILE copy, so a previous DB that was not
    // checkpointed/closed cleanly (a leftover WAL/SHM sidecar) could lose its
    // unflushed rows in the copy. Fall back to the full load rather than
    // publish an incomplete database (task-1 checkpoint guard).
    for suffix in [".wal", ".shm"] {
        let sidecar = PathBuf::from(format!("{}{suffix}", db.display()));
        if sidecar.exists() {
            log.ln(&format!(
                "[load] splice: previous db.lbug has a {suffix} sidecar (not cleanly closed) — full load"
            ));
            return None;
        }
    }
    // The delta refreshes the single Scan row; without the ingested scan_meta
    // there is nothing to write, so fall back rather than publish a bogus head.
    let Some(scan) = scan_row_from_graph(graph) else {
        log.ln("[load] splice: assembled graph carries no Scan row — full load");
        return None;
    };
    // The delete scope is the FINAL phase-2 target set (stage-1 ∪ the signature
    // cascade). The assembled graph carries repo-relative identities now, so the
    // delete scope includes each target's repo-relative identity verbatim;
    // the absolute spelling is also included so a graph built from absolute
    // fixture paths still matches.
    let targets: BTreeSet<String> = input
        .targets_rel
        .iter()
        .flat_map(|rel| [rel.clone(), incremental::absolute(&input.scan_root, rel)])
        .collect();

    let seeded = match splice::seed_checked(&db, input.recorded_content_key.as_deref()) {
        splice::SeedDecision::Seed(seeded) => seeded,
        splice::SeedDecision::FullLoad(reason) => {
            log.ln(&format!("[load] splice: {} — full load", reason.describe()));
            return None;
        }
    };

    let delta = splice::SpliceDelta {
        graph,
        targets: &targets,
        removed_fqns: &input.removed_fqns,
        scan,
    };
    let report = match seeded.apply(&delta) {
        Ok(report) => report,
        Err(e) => {
            // A mid-sequence failure leaves the seeded COPY partially mutated;
            // discard it and hand the caller to the full load. The previous DB
            // was only ever read, so it is untouched.
            log.ln(&format!(
                "[load] splice: delta application failed ({e:#}) — discarding seed, full load"
            ));
            let _ = seeded.discard();
            return None;
        }
    };

    match splice::publish(seeded, graph, &splice::export_path(apg_root)) {
        Ok(()) => Some(report),
        Err(e) => {
            // `publish` rolls both targets back to their previous bytes on a
            // reported failure, so the full load starts from a clean pair.
            log.ln(&format!(
                "[load] splice: publish failed ({e:#}) — falling back to the full load"
            ));
            None
        }
    }
}

/// The `ScanRow` the splice refreshes, read from the assembled graph's single
/// `Scan` node (the ingested `scan_meta`). `None` when the graph carries no
/// scan_meta, which makes the splice ineligible.
fn scan_row_from_graph(graph: &graph::Graph) -> Option<splice::ScanRow> {
    let node = graph.nodes.get(schema::SCAN_HEAD)?;
    Some(splice::ScanRow {
        git_sha: node.git_sha.clone(),
        git_clean: node.git_clean,
        content_key: node.content_key.clone(),
        scanned_at: node.scanned_at.clone().unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests;
