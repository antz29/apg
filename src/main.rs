mod artifacts;
mod cache;
mod classify;
mod cleanup;
mod delta;
mod git;
mod graph;
mod impact;
mod incremental;
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
mod splice;
#[cfg(test)]
mod testutil;
mod timing;
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
fn auto_detect_languages(dir: &std::path::Path, available: &[String]) -> Vec<String> {
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
pub(crate) struct FrontendHandoff {
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
    fn write_targets(&self, tmp: &Path, lang: &str, targets: &[String]) -> PathBuf {
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
    fn append(&self, child: &mut Command, tmp: &Path, lang: &str, targets: &[String]) {
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
fn targets_for_language(
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
#[cfg(test)]
pub(crate) fn read_targets_file(path: &Path) -> Vec<String> {
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
fn try_splice_build(
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
mod tests {
    use super::*;

    /// The version this release gate guards. Bump this literal in lockstep with
    /// the `[package] version` line on every release: the assertions below fail
    /// on any drift (manifest/lockfile/compiled constant ahead of or behind the
    /// advertised release), so a bump commit cannot silently skip it.
    const RELEASE_VERSION: &str = "0.17.0";

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

    /// A scratch repo with real Go sources, a `go.mod`, and an `apg/` layout at
    /// the binary's version. Returns `(base, repo_dir)` (base for teardown).
    fn winb_scratch(tag: &str, files: &[(&str, &str)]) -> (PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!("apg-winb-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let repo_dir = base.join("repo");
        let home = base.join("home");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(home.join(".opencode/node_modules/@opencode-ai/plugin")).unwrap();
        scratch_repo_init(&repo_dir);
        std::fs::write(repo_dir.join("go.mod"), "module scratch\n\ngo 1.21\n").unwrap();
        for (rel, body) in files {
            let p = repo_dir.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        }
        scratch_commit_all(&repo_dir, "init source");
        (base, repo_dir)
    }

    /// The standard three-package Go fixture: `a` (leaf), `b` (depends on a),
    /// `c` (depends on b).
    fn winb_go_fixture() -> Vec<(&'static str, &'static str)> {
        vec![
            (
                "a/a.go",
                "package a\n\n// A is a struct.\ntype A struct {\n\tX int\n}\n\n// Leaf is the leaf function.\nfunc Leaf() int { return 1 }\n",
            ),
            (
                "b/b.go",
                "package b\n\nimport \"scratch/a\"\n\n// B is a struct.\ntype B struct {\n\tA a.A\n}\n\n// Foo calls the leaf.\nfunc Foo() int { return a.Leaf() }\n",
            ),
            (
                "c/c.go",
                "package c\n\nimport \"scratch/b\"\n\n// Bar calls Foo.\nfunc Bar() int { return b.Foo() }\n",
            ),
        ]
    }

    fn winb_run(repo_dir: &Path, home: &Path, args: &[&str]) -> std::process::Output {
        testutil::ApgCommand::new(args)
            .cwd(repo_dir)
            .env("HOME", &home.to_string_lossy())
            .output()
    }

    /// Parse the export into comparable node/edge/unresolved sets.
    fn winb_graph(
        repo_dir: &Path,
    ) -> (
        BTreeSet<String>,
        BTreeSet<(String, String)>,
        BTreeSet<String>,
    ) {
        let text = std::fs::read_to_string(repo_dir.join("apg/.trans/graph.jsonl")).unwrap();
        let mut nodes = BTreeSet::new();
        let mut edges = BTreeSet::new();
        let mut unresolved = BTreeSet::new();
        for line in text.lines() {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            let ty = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
            match ty {
                "module" | "file" | "struct" | "function" => {
                    nodes.insert(format!(
                        "{}:{}",
                        ty,
                        v.get("fqn").and_then(|f| f.as_str()).unwrap_or("")
                    ));
                }
                "contains" | "calls" | "uses" | "unresolved_call" | "unresolved_use" => {
                    edges.insert((
                        ty.to_string(),
                        format!(
                            "{}->{}",
                            v.get("from").and_then(|f| f.as_str()).unwrap_or(""),
                            v.get("to").and_then(|f| f.as_str()).unwrap_or("")
                        ),
                    ));
                }
                "unresolved" => {
                    unresolved.insert(format!(
                        "{}:{}",
                        v.get("fqn").and_then(|f| f.as_str()).unwrap_or(""),
                        v.get("category").and_then(|c| c.as_str()).unwrap_or("")
                    ));
                }
                _ => {}
            }
        }
        (nodes, edges, unresolved)
    }

    // -----------------------------------------------------------------------
    // Phase-05 Rust all-manifest discovery / isolation acceptance helpers
    // (tasks 5, 6, 9). Non-#[test] helpers, so they live at the `mod tests`
    // root.
    // -----------------------------------------------------------------------

    /// Parses a scan's `apg/.trans/graph.jsonl` export into raw JSON records.
    fn export_records(repo_dir: &Path) -> Vec<serde_json::Value> {
        let path = repo_dir.join("apg/.trans/graph.jsonl");
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
            .collect()
    }

    /// Module FQN → number of module records carrying it. A count above 1 means
    /// the same project was emitted twice — the workspace double-count that the
    /// dedup by manifest root must prevent.
    fn export_module_counts(
        records: &[serde_json::Value],
    ) -> std::collections::BTreeMap<String, usize> {
        let mut counts = std::collections::BTreeMap::new();
        for r in records {
            if r.get("type").and_then(|t| t.as_str()) == Some("module")
                && let Some(fqn) = r.get("fqn").and_then(|f| f.as_str())
            {
                *counts.entry(fqn.to_string()).or_insert(0) += 1;
            }
        }
        counts
    }

    /// Every code-node location in the export (a File's fqn is its absolute
    /// path; a Struct/Function carries `path`), so a caller can assert no node
    /// is drawn from a generated/dependency tree.
    fn export_code_locations(records: &[serde_json::Value]) -> Vec<String> {
        let mut out = Vec::new();
        for r in records {
            match r.get("type").and_then(|t| t.as_str()) {
                Some("file") => {
                    if let Some(v) = r.get("fqn").and_then(|v| v.as_str()) {
                        out.push(v.to_string());
                    }
                }
                Some("struct") | Some("function") => {
                    if let Some(v) = r.get("path").and_then(|v| v.as_str()) {
                        out.push(v.to_string());
                    }
                }
                _ => {}
            }
        }
        out
    }

    /// The Struct/Function FQNs in the export.
    fn export_symbol_fqns(records: &[serde_json::Value]) -> BTreeSet<String> {
        records
            .iter()
            .filter(|r| {
                matches!(
                    r.get("type").and_then(|t| t.as_str()),
                    Some("struct") | Some("function")
                )
            })
            .filter_map(|r| r.get("fqn").and_then(|f| f.as_str()).map(str::to_string))
            .collect()
    }

    /// True when `path` (absolute) has a directory component named `name` BELOW
    /// `root` — the discovery/emission exclusion predicate, anchored at the scan
    /// root so the root path's own leading components never match.
    fn under_component(root: &Path, path: &str, name: &str) -> bool {
        let rel = Path::new(path)
            .strip_prefix(root)
            .unwrap_or(Path::new(path));
        rel.components()
            .any(|c| matches!(c, std::path::Component::Normal(s) if s == name))
    }

    /// P9 roots a module/symbol FQN under a `<lang>.` segment; strip that root
    /// so an assertion holds both pre-rooting (bare) and post-rooting. Module
    /// identities here are `apg` / `apg-rustfrontend` (no dot), their symbols
    /// `apg.X` / `apg-rustfrontend.X`.
    fn strip_lang_root(fqn: &str) -> String {
        match fqn.split_once('.') {
            Some(("rust", rest)) => rest.to_string(),
            _ => fqn.to_string(),
        }
    }

    /// The rust frontend binary the candidate stages, resolved from
    /// `testutil::apg_bin()`'s profile dir (`<profile>/frontends/rustfrontend`)
    /// — the same artifact `apg scan` spawns. Fails loudly naming the build.
    fn rust_frontend_bin() -> PathBuf {
        let apg = crate::testutil::apg_bin();
        let bin = apg
            .parent()
            .expect("apg binary has a parent")
            .join("frontends")
            .join("rustfrontend");
        assert!(
            bin.is_file(),
            "rust frontend not found at {} — build it first: \
             cargo build --config 'env.APG_BUILD_FRONTENDS=\"rust\"'",
            bin.display()
        );
        bin
    }

    /// Runs the rust frontend directly over `repo_dir` and parses its emitted
    /// unified-schema records — the fixture's scanner spool, fed to the
    /// in-process ingestor (the in-process shadow counters cannot be observed
    /// from the CLI's warning line alone).
    fn rust_frontend_records(repo_dir: &Path) -> Vec<crate::schema::Record> {
        let bin = rust_frontend_bin();
        let out = std::process::Command::new(&bin)
            .arg(repo_dir)
            .output()
            .unwrap_or_else(|e| panic!("spawn {}: {e}", bin.display()));
        assert!(
            out.status.success(),
            "rustfrontend failed over {}: {}",
            repo_dir.display(),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| {
                serde_json::from_str::<crate::schema::Record>(l)
                    .unwrap_or_else(|e| panic!("bad scanner record `{l}`: {e}"))
            })
            .collect()
    }

    // -----------------------------------------------------------------------
    // Phase-07 Markdown frontend acceptance helpers (task-13), retargeted onto
    // the bundled structural scanner (apg-0.17.0 phase-04 task-8). Non-#[test]
    // helpers, so they live at the `mod tests` root.
    // -----------------------------------------------------------------------

    /// The staged bundled structural scanner the candidate runs
    /// (`<profile>/frontends/structfrontend`), resolved from
    /// `testutil::apg_bin()` — the same artifact `apg scan` spawns. It serves
    /// the `md` stream (the absorbed Markdown frontend) among every structural
    /// stream, so the Markdown acceptance drives it with `--stream md`. Fails
    /// loudly naming the build, like [`rust_frontend_bin`].
    fn md_frontend_bin() -> PathBuf {
        let apg = crate::testutil::apg_bin();
        let bin = apg
            .parent()
            .expect("apg binary has a parent")
            .join("frontends")
            .join("structfrontend");
        assert!(
            bin.is_file(),
            "structural frontend not found at {} — build it first: \
             cargo build --config 'env.APG_BUILD_FRONTENDS=\"struct\"'",
            bin.display()
        );
        bin
    }

    /// Runs the bundled structural scanner's `md` stream directly over
    /// `repo_dir` and parses its emitted unified-schema records — the fixture's
    /// scanner spool, fed to the in-process ingestor so the shadow counters can
    /// be observed POSITIVELY (a real scan only prints a warning when they are
    /// non-zero, so the warning's absence alone would be vacuous). `--stream md`
    /// preserves the retired `mdfrontend`'s md-only emission (the one binary
    /// otherwise emits every structural stream).
    fn md_frontend_records(repo_dir: &Path) -> Vec<crate::schema::Record> {
        let bin = md_frontend_bin();
        let out = std::process::Command::new(&bin)
            .arg(repo_dir)
            .args(["--stream", "md"])
            .output()
            .unwrap_or_else(|e| panic!("spawn {}: {e}", bin.display()));
        assert!(
            out.status.success(),
            "structfrontend (md stream) failed over {}: {}",
            repo_dir.display(),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| {
                serde_json::from_str::<crate::schema::Record>(l)
                    .unwrap_or_else(|e| panic!("bad scanner record `{l}`: {e}"))
            })
            .collect()
    }

    /// A scratch git repo for the Markdown acceptance fixture: the caller's
    /// files, an isolated HOME (the suite dependency dir pre-created so `apg
    /// init` never shells out to npm), `apg init`, and both commits. Returns
    /// `(base, repo_dir, home)`.
    fn md_scratch(tag: &str, files: &[(&str, &str)]) -> (PathBuf, PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!("apg-md-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let repo_dir = base.join("repo");
        let home = base.join("home");
        std::fs::create_dir_all(home.join(".opencode/node_modules/@opencode-ai/plugin")).unwrap();
        scratch_repo_init(&repo_dir);
        for (rel, body) in files {
            let p = repo_dir.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        }
        scratch_commit_all(&repo_dir, "init source");
        let init = testutil::ApgCommand::new(&["init", "."])
            .cwd(&repo_dir)
            .env("HOME", home.to_str().unwrap())
            .output();
        assert!(
            init.status.success(),
            "apg init failed in {}: {}{}",
            repo_dir.display(),
            String::from_utf8_lossy(&init.stdout),
            String::from_utf8_lossy(&init.stderr)
        );
        scratch_commit_all(&repo_dir, "apg init");
        (base, repo_dir, home)
    }

    /// The phase-07 Markdown acceptance fixture: a mixed Go + Markdown repo
    /// carrying every pinned AC shape — nested duplicate headings, a sibling
    /// after a deeper heading (nearest preceding lower-level parent), a
    /// heading-less document, same-stem `README.md`/`README.markdown` in one
    /// directory, same-stem files in different directories, a subdirectory
    /// module, an ignored `.mdx`, and a `gen/` document classified generated.
    fn md_mixed_fixture() -> Vec<(&'static str, &'static str)> {
        vec![
            ("go.mod", "module scratch\n\ngo 1.21\n"),
            ("main.go", "package main\n\nfunc main() {}\n"),
            (
                "docs/overview.md",
                "# Overview\n\n## Overview\n\n### Overview 1\n",
            ),
            (
                "docs/nesting.md",
                "# Title\n\n## Child\n\n### Grandchild\n\n## Sibling\n",
            ),
            ("docs/empty.md", "Just prose, with no headings at all.\n"),
            ("docs/intro.md", "# Intro\n"),
            ("guides/intro.md", "# Intro\n"),
            ("docs/deep/page.md", "# Deep\n"),
            // The requirement's path-alias fixture: dot-joining a directory
            // path would collapse `docs/guide` and `docs.guide` to one prefix.
            ("docs/guide/x.md", "# X\n"),
            ("docs.guide/x.md", "# X\n"),
            ("notes/README.md", "# Notes\n"),
            ("notes/README.markdown", "# Notes\n"),
            ("notes/draft.mdx", "# Draft\n"),
            ("onlymdx/draft.mdx", "# Draft\n"),
            ("gen/generated.md", "# Generated\n"),
        ]
    }

    /// Builds a real on-disk DB at `path` through the same
    /// `create_schema + copy_from` full load the scan path uses, so the dispatch
    /// under test sees a genuine previous database.
    fn win_c_build_db(path: &Path, graph: &graph::Graph) {
        let ldir = path.parent().unwrap().join("load");
        std::fs::create_dir_all(&ldir).unwrap();
        load::build_load_files(graph, &ldir).unwrap();
        let db = Database::new(path, SystemConfig::default()).unwrap();
        let conn = Connection::new(&db).unwrap();
        load::create_schema(&conn).unwrap();
        load::copy_from(&conn, &ldir).unwrap();
        drop(conn);
        drop(db);
    }

    /// A previous/next graph for the dispatch fixtures: the module, the target
    /// file, a struct and one function, plus the single `Scan` head.
    fn win_c_fixture(abs_file: &str, scan_sha: &str) -> graph::Graph {
        use crate::graph::{Graph, Location, Node, NodeKind};
        let located = |kind: NodeKind| Node {
            kind,
            location: Some(Location {
                path: PathBuf::from(abs_file),
                start: 0,
                end: 1,
                start_line: 1,
                end_line: 1,
            }),
            ..Node::default()
        };
        let mut g = Graph::default();
        g.nodes.insert(
            "mod".to_string(),
            Node {
                kind: NodeKind::Module,
                ..Node::default()
            },
        );
        g.nodes
            .insert(abs_file.to_string(), located(NodeKind::File));
        g.nodes
            .insert("mod.A".to_string(), located(NodeKind::Struct));
        g.nodes
            .insert("mod.A.f".to_string(), located(NodeKind::Function));
        g.nodes.insert(
            schema::SCAN_HEAD.to_string(),
            Node {
                kind: NodeKind::Scan,
                git_sha: Some(scan_sha.to_string()),
                content_key: Some(format!("key-{scan_sha}")),
                scanned_at: Some(format!("t-{scan_sha}")),
                ..Node::default()
            },
        );
        g.contains.insert(("mod".to_string(), abs_file.to_string()));
        g.contains
            .insert((abs_file.to_string(), "mod.A".to_string()));
        g.contains
            .insert(("mod.A".to_string(), "mod.A.f".to_string()));
        g
    }

    /// The incremental `PipelineInput` the dispatch fixtures pass: the win-B
    /// reuse plan (the eligibility marker) plus the phase-2 target set.
    fn win_c_input(
        base: &Path,
        scan_root: &Path,
        targets_rel: &[&str],
    ) -> incremental::PipelineInput {
        use crate::cache::{CacheKey, Manifest, ScanConfigKey};
        let key = CacheKey::compute(&ScanConfigKey::default());
        incremental::PipelineInput {
            store_root: Some(base.join("store")),
            cache_key: key.clone(),
            scan_root: scan_root.to_path_buf(),
            manifest: Manifest::default(),
            sha: "new".to_string(),
            reuse: Some(incremental::ReusePlan {
                store_root: base.join("store"),
                cache_key: key,
                files: Vec::new(),
                reader_root: scan_root.to_string_lossy().into_owned(),
                skipped_langs: BTreeSet::new(),
            }),
            targets_rel: targets_rel.iter().map(|s| s.to_string()).collect(),
            removed_fqns: BTreeSet::new(),
            recorded_content_key: Some("key-old".to_string()),
        }
    }

    // -----------------------------------------------------------------------
    // Phase-04 Java targeted-emission / class-cache helpers (tasks 23/25).
    // Non-#[test] helpers, so they live at the `mod tests` root.
    // -----------------------------------------------------------------------

    /// A scratch /tmp git repo holding a Java fixture plus an isolated
    /// `APG_FRONTEND_DIR` carrying ONLY the candidate's staged `java-classes`
    /// (so auto-detection yields exactly `java`, never every installed
    /// frontend). Returns `(base, repo_dir, home, frontend_dir)`; the caller
    /// removes `base` at teardown. Candidate binary only — never a real project
    /// (`global.constraint.no-real-project-test`).
    fn java_scratch(tag: &str, files: &[(&str, &str)]) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!("apg-java-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let repo_dir = base.join("repo");
        let home = base.join("home");
        std::fs::create_dir_all(&home).unwrap();
        // Keep `apg init` hermetic/fast: pre-create the opencode plugin dir so
        // it never shells out to npm.
        std::fs::create_dir_all(home.join(".opencode/node_modules/@opencode-ai/plugin")).unwrap();
        scratch_repo_init(&repo_dir);
        for (rel, body) in files {
            let p = repo_dir.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        }
        scratch_commit_all(&repo_dir, "init source");
        // The isolated java-only frontend dir (the candidate's staged artifact).
        let frontend_dir = base.join("frontends");
        let src = testutil::apg_bin()
            .parent()
            .expect("apg binary parent")
            .join("frontends")
            .join("java-classes");
        assert!(
            src.is_dir(),
            "the staged Java frontend must exist at {} — run `cargo build` first",
            src.display()
        );
        testutil::copy_dir(&src, &frontend_dir.join("java-classes"))
            .unwrap_or_else(|e| panic!("stage the isolated java frontend: {e:#}"));
        (base, repo_dir, home, frontend_dir)
    }

    /// Runs the candidate `apg` against a java scratch repo with the isolated
    /// `HOME` + java-only `APG_FRONTEND_DIR`.
    fn java_run(
        repo_dir: &Path,
        home: &Path,
        frontend_dir: &Path,
        args: &[&str],
    ) -> std::process::Output {
        testutil::ApgCommand::new(args)
            .cwd(repo_dir)
            .env("HOME", &home.to_string_lossy())
            .env("APG_FRONTEND_DIR", &frontend_dir.to_string_lossy())
            .output()
    }

    /// The shared `<git-common-dir>/apg/facts` store for a scratch repo.
    fn java_store(repo_dir: &Path) -> PathBuf {
        repo_dir.join(".git/apg/facts")
    }

    /// The enumerated per-rel-type oracle: rel type -> record count for the four
    /// Java rel types the task names (`Calls`/`Uses`/`UnresolvedCall`/
    /// `UnresolvedUse`, i.e. wire `calls`/`uses`/`unresolved_call`/
    /// `unresolved_use`). Every type is present with `0` when it has no records,
    /// so a dropped rel type is a divergence rather than an absent key.
    fn java_rel_counts(repo_dir: &Path) -> std::collections::BTreeMap<String, usize> {
        let text = std::fs::read_to_string(repo_dir.join("apg/.trans/graph.jsonl")).unwrap();
        let mut counts = std::collections::BTreeMap::new();
        for ty in ["calls", "uses", "unresolved_call", "unresolved_use"] {
            counts.insert(ty.to_string(), 0usize);
        }
        for line in text.lines() {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            let ty = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
            if let Some(c) = counts.get_mut(ty) {
                *c += 1;
            }
        }
        counts
    }

    /// The `UnresolvedTarget` set by FQN -> category (the task's oracle: the set
    /// must be EQUAL by FQN *with* categories, not by FQN alone).
    fn java_unresolved(repo_dir: &Path) -> std::collections::BTreeMap<String, String> {
        let text = std::fs::read_to_string(repo_dir.join("apg/.trans/graph.jsonl")).unwrap();
        let mut out = std::collections::BTreeMap::new();
        for line in text.lines() {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            if v.get("type").and_then(|t| t.as_str()) != Some("unresolved") {
                continue;
            }
            let fqn = v
                .get("fqn")
                .and_then(|f| f.as_str())
                .unwrap_or("")
                .to_string();
            let cat = v
                .get("category")
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .to_string();
            if let Some(prev) = out.insert(fqn.clone(), cat.clone()) {
                assert_eq!(prev, cat, "the unresolved target {fqn} has two categories");
            }
        }
        out
    }

    /// The `(from, to)` endpoints of one edge kind in the export
    /// (`calls`/`uses`/…) — the positive half of the task-23 extension, where
    /// the re-emitted file's references into the TARGET package must appear as
    /// their real resolved FQNs (never a bare simple name or an error symbol).
    fn java_edges(repo_dir: &Path, kind: &str) -> std::collections::BTreeSet<(String, String)> {
        let text = std::fs::read_to_string(repo_dir.join("apg/.trans/graph.jsonl")).unwrap();
        let mut out = std::collections::BTreeSet::new();
        for line in text.lines() {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            if v.get("type").and_then(|t| t.as_str()) != Some(kind) {
                continue;
            }
            let from = v.get("from").and_then(|f| f.as_str()).unwrap_or("");
            let to = v.get("to").and_then(|t| t.as_str()).unwrap_or("");
            out.insert((from.to_string(), to.to_string()));
        }
        out
    }

    /// The simple names of every project `struct` the scan declared (the
    /// "project class" set the bare-name leak is checked against).
    fn java_project_class_simple_names(repo_dir: &Path) -> std::collections::BTreeSet<String> {
        let text = std::fs::read_to_string(repo_dir.join("apg/.trans/graph.jsonl")).unwrap();
        let mut out = std::collections::BTreeSet::new();
        for line in text.lines() {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            if v.get("type").and_then(|t| t.as_str()) != Some("struct") {
                continue;
            }
            if let Some(fqn) = v.get("fqn").and_then(|f| f.as_str())
                && let Some(simple) = fqn.rsplit('.').next()
            {
                out.insert(simple.to_string());
            }
        }
        out
    }

    /// True when the shared store holds a Java CLASS-cache artifact — the
    /// `<cache-dir>/java/<cache-key>/surface.tsv` (or `classes/` dir) the
    /// targeted path consumes. The `<cache-dir>/java/<cache-key>/` directory
    /// itself is ALSO the FactStore's per-language fact-unit bucket, so its mere
    /// existence is not a class-cache artifact.
    fn java_class_cache_seeded(store: &Path) -> bool {
        let java = store.join("java");
        let Ok(keys) = std::fs::read_dir(&java) else {
            return false;
        };
        for k in keys.flatten() {
            let d = k.path();
            if d.join("surface.tsv").exists() || d.join("classes").is_dir() {
                return true;
            }
        }
        false
    }

    /// Recursively collects every Java class-cache artifact (`surface.tsv` file
    /// or `classes/` directory) under `root`. The task-25 no-hand-off control
    /// uses it to catch a fabricated/defaulted cache path ANYWHERE in the
    /// scratch tree, not merely under the shared store.
    fn collect_class_cache_artifacts(root: &Path, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(root) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name == "surface.tsv" || (name == "classes" && p.is_dir()) {
                out.push(p.display().to_string());
            }
            if p.is_dir() {
                collect_class_cache_artifacts(&p, out);
            }
        }
    }

    /// The `N` of a Java frontend `compiling N unchanged-package file(s)` line,
    /// or `None` when the log carries no such line (i.e. nothing was rebuilt).
    fn java_unchanged_package_compiles(log: &str) -> Option<usize> {
        for line in log.lines() {
            let Some(rest) = line.split("compiling ").nth(1) else {
                continue;
            };
            if !rest.contains("unchanged-package") {
                continue;
            }
            return rest.split_whitespace().next().and_then(|s| s.parse().ok());
        }
        None
    }

    /// The number of `.class` files anywhere under the shared store's Java class
    /// caches (`<store>/java/<cache-key>/classes`). The task-23 nested-root
    /// fixture carries a source javac cannot compile (`pkg.a.Broken`), so the
    /// compile batch cannot emit bytecode and the count is ZERO — the positive
    /// proof that the targeted scan's exact facts came from the corrected
    /// `-sourcepath` (the actual source roots), NOT from a bytecode cache. A
    /// non-zero count would mean the fixture could pass vacuously via the
    /// classpath, so the test asserts this observable.
    fn java_class_file_count(store: &Path) -> usize {
        let java = store.join("java");
        let Ok(keys) = std::fs::read_dir(&java) else {
            return 0;
        };
        keys.flatten()
            .map(|k| count_class_files(&k.path().join("classes")))
            .sum()
    }

    /// Recursively counts `.class` files under a class dir.
    fn count_class_files(dir: &Path) -> usize {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return 0;
        };
        let mut n = 0usize;
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                n += count_class_files(&p);
            } else if p.extension().and_then(|x| x.to_str()) == Some("class") {
                n += 1;
            }
        }
        n
    }

    /// The task-23 Java fixture (EXTENDED, feedback-134; nested-root corrected
    /// per phase-04 task-23): a changed package (`pkg.b`) whose type is
    /// referenced from an unchanged package (`pkg.c`) and which itself calls
    /// into an unchanged package (`pkg.a`) — the cross-package resolution shape
    /// note-87's divergence class exercised on jgrapht (resolved -> unresolved
    /// when the context is incomplete).
    ///
    /// FIXTURE ROOT (REQUIRED): every source sits under a MAVEN-LIKE NESTED
    /// SOURCE ROOT — `proj/src/main/java/pkg/...` under the scan root — so the
    /// scan root is NOT a valid package root, exactly like jgrapht's Maven
    /// multi-module layout (`<root>/jgrapht-core/src/main/java/org/jgrapht/…`).
    /// This is what makes the pre-fix `-sourcepath <scan root>` silently
    /// INEFFECTIVE: javac looks for `<scan root>/pkg/a/A.java`, which does not
    /// exist, so a reference into a non-target package degrades to a javac
    /// error symbol. A fixture with `pkg/a/A.java` DIRECTLY under the scan root
    /// does NOT reproduce the divergence (the scan root WAS a valid package
    /// root there) and is exactly the wrong assumption the committed frontend
    /// fixture encoded before task-15's smallest falsifiable experiment.
    ///
    /// On top of the nested root, the fixture carries:
    ///
    /// (a) `pkg.b.Target` — a TARGET-package declaration, declared in the
    ///     re-emitted package and referenced from the re-emitted `pkg.b.B` — so
    ///     the complete project-class index must cover the TARGET declarations
    ///     too, not the unchanged-package surface alone (task-15/-35);
    /// (b) `pkg.a.Broken` — a source javac cannot compile (it names the absent
    ///     package `missing`), dropped by `compileAndCollect`'s single-file
    ///     catch; its declarations must still contribute to the surface/class
    ///     dir, and (with the batch unable to emit bytecode) the class dir it
    ///     leaves behind is incomplete — the degraded-context shape the
    ///     frontend-level fixture (`CallGraphBuilderTest
    ///     .writeIncompleteContextFixture`) demonstrates as pre-fix FAIL /
    ///     post-fix PASS; and
    /// (c) the NON-TARGET `pkg.a` (and `pkg.c`) packages the target package
    ///     references, in the nested-root layout, so the missing non-target
    ///     resolution context actually bites.
    fn java_edge_exactness_fixture() -> Vec<(&'static str, &'static str)> {
        vec![
            (
                "proj/src/main/java/pkg/a/A.java",
                "package pkg.a;\n\npublic class A {\n    public int foo() { return 1; }\n}\n",
            ),
            (
                "proj/src/main/java/pkg/a/Util.java",
                "package pkg.a;\n\npublic class Util {\n    public static int twice(int n) { return n * 2; }\n}\n",
            ),
            // The un-attributable source: javac errors on `missing.Thing`, so
            // the batch cannot emit bytecode and the class dir is incomplete.
            (
                "proj/src/main/java/pkg/a/Broken.java",
                "package pkg.a;\n\npublic class Broken {\n    public missing.Thing boom() { return null; }\n}\n",
            ),
            // The TARGET-package declaration the re-emitted `pkg.b.B` uses: a
            // class whose simple name must never leak as an UnresolvedTarget.
            (
                "proj/src/main/java/pkg/b/Target.java",
                "package pkg.b;\n\npublic class Target {\n    public Target() {}\n\n    public int t() { return 2; }\n}\n",
            ),
            (
                "proj/src/main/java/pkg/b/B.java",
                "package pkg.b;\n\nimport pkg.a.A;\nimport pkg.a.Util;\n\npublic class B {\n    private final A a = new A();\n    private final Target t = new Target();\n\n    public int bar() { return Util.twice(a.foo()) + t.t(); }\n\n    public Target make() { return new Target(); }\n}\n",
            ),
            (
                "proj/src/main/java/pkg/c/C.java",
                "package pkg.c;\n\nimport pkg.b.B;\n\npublic class C {\n    public int baz() { return new B().bar(); }\n}\n",
            ),
            (
                "proj/src/main/java/pkg/c/ListUser.java",
                "package pkg.c;\n\nimport java.util.ArrayList;\nimport java.util.List;\n\npublic class ListUser {\n    public int size() {\n        List<String> xs = new ArrayList<>();\n        xs.add(\"x\");\n        return xs.size();\n    }\n}\n",
            ),
        ]
    }

    /// The task-25 fixture: a small target package (`pkg.target`) with several
    /// UNCHANGED packages around it, so a first targeted scan that still has to
    /// rebuild the class cache compiles a large fraction of the fixture (the
    /// pre-fix `compiling N unchanged-package file(s)` failure).
    fn java_seeding_fixture() -> Vec<(&'static str, &'static str)> {
        vec![
            (
                "pkg/target/T.java",
                "package pkg.target;\n\nimport pkg.dep.Dep;\n\npublic class T {\n    public int go() { return new Dep().v(); }\n}\n",
            ),
            (
                "pkg/dep/Dep.java",
                "package pkg.dep;\n\npublic class Dep {\n    public int v() { return 1; }\n}\n",
            ),
            (
                "pkg/x/X1.java",
                "package pkg.x;\n\npublic class X1 {\n    public int one() { return 1; }\n}\n",
            ),
            (
                "pkg/x/X2.java",
                "package pkg.x;\n\npublic class X2 {\n    public int two() { return 2; }\n}\n",
            ),
            (
                "pkg/y/Y1.java",
                "package pkg.y;\n\npublic class Y1 {\n    public int one() { return 1; }\n}\n",
            ),
            (
                "pkg/y/Y2.java",
                "package pkg.y;\n\npublic class Y2 {\n    public int two() { return 2; }\n}\n",
            ),
            (
                "pkg/z/Z1.java",
                "package pkg.z;\n\npublic class Z1 {\n    public int one() { return 1; }\n}\n",
            ),
            (
                "pkg/z/Z2.java",
                "package pkg.z;\n\npublic class Z2 {\n    public int two() { return 2; }\n}\n",
            ),
        ]
    }

    /// unit tier -- pure in-memory: no filesystem, database, git or process.
    mod unit {
        use super::*;

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

        /// Phase-03 task-5: the per-language spawn verdict. On the incremental
        /// path, in the PARTIAL case (the scan has other changed languages) a
        /// language whose target set is empty is SKIPPED entirely (no process
        /// spawn), while a language with targets spawns. When the whole target set
        /// is empty phase-02's unfiltered path runs every language, and on a full
        /// scan every language spawns. The verdict is derived from the SAME
        /// phase-2 `targets_rel` set that drives the win-C DB splice, via
        /// `targets_for_language`, never a fresh detection walk.
        #[test]
        fn spawn_verdict_skips_unchanged_languages_only_on_the_incremental_path() {
            // A partial (win-C) delta: go changed, ts untouched.
            let mut rel = BTreeSet::new();
            rel.insert("a/a.go".to_string());
            let go_targets = targets_for_language(&rel, Path::new("/root"), "go");
            let ts_targets = targets_for_language(&rel, Path::new("/root"), "ts");
            let any = !rel.is_empty();

            // Incremental PARTIAL case: the changed language spawns; the unchanged
            // one is skipped entirely (not merely emission-filtered).
            assert!(should_spawn_language(
                false,
                go_targets.is_empty(),
                any,
                false
            ));
            assert!(!should_spawn_language(
                false,
                ts_targets.is_empty(),
                any,
                false
            ));

            // Incremental with NO targets anywhere: phase-02's unfiltered path runs
            // every language (an empty target file means "no filter"), so the
            // frontends still emit their global module scaffolding + full universe.
            assert!(should_spawn_language(false, true, false, false));
            assert!(should_spawn_language(false, false, false, false));

            // Full scan: every detected/requested language still spawns, even with
            // an empty target set (no emission filter is passed on this path).
            assert!(should_spawn_language(true, true, false, false));
            assert!(should_spawn_language(
                true,
                go_targets.is_empty(),
                any,
                false
            ));

            // phase-06 task-2: a COMPLETE warm cache skips EVERY language even on
            // the empty-target case phase-02's unfiltered path would otherwise
            // spawn — the recorded facts + scaffolding reconstruct the graph.
            assert!(!should_spawn_language(false, true, false, true));
            assert!(!should_spawn_language(false, false, false, true));
            // A full scan still spawns every language (the warm flag never
            // overrides the correctness full-scan gate).
            assert!(should_spawn_language(true, true, false, true));

            // Phase-08 task-23: the `py` scan id receives its `.py`/`.pyi`
            // targets — `language_of` renders the scan-side token `py`, not the
            // stale `python`, so the filter matches.
            let mut py_rel = BTreeSet::new();
            py_rel.insert("pkg/a.py".to_string());
            py_rel.insert("pkg/stub.pyi".to_string());
            assert_eq!(
                targets_for_language(&py_rel, Path::new("/root"), "py"),
                vec![
                    "/root/pkg/a.py".to_string(),
                    "/root/pkg/stub.pyi".to_string(),
                ],
                "the py scan id must receive its .py/.pyi targets"
            );
            assert!(
                targets_for_language(&py_rel, Path::new("/root"), "go").is_empty(),
                "a py target must not be delivered to another language"
            );

            // Phase-08 task-24: the unified js/ts artifact is ONE `language_of`
            // bucket under TWO scan ids. A JS-only repo scans under `js`, and a
            // TS-containing repo under `ts`; both must receive the js-family
            // targets even though `language_of` renders the single token `ts`.
            let mut js_rel = BTreeSet::new();
            js_rel.insert("web/app.js".to_string());
            js_rel.insert("web/widget.jsx".to_string());
            js_rel.insert("web/esm.mjs".to_string());
            js_rel.insert("web/cjs.cjs".to_string());
            js_rel.insert("web/typed.ts".to_string());
            for id in ["js", "ts"] {
                let got = targets_for_language(&js_rel, Path::new("/root"), id);
                for suffix in ["web/app.js", "web/widget.jsx", "web/esm.mjs", "web/cjs.cjs"] {
                    assert!(
                        got.iter().any(|p| p.ends_with(suffix)),
                        "the `{id}` scan id must receive its {suffix} target: {got:?}"
                    );
                }
            }
            assert!(
                targets_for_language(&js_rel, Path::new("/root"), "go").is_empty(),
                "a js/ts target must not be delivered to another language"
            );
        }

        /// Phase-03 task-5: the pure scan-time toolchain mapping and the named,
        /// actionable error text. The mapping is exactly the per-language
        /// scan-time tool set (`go`; a JDK's `java`; `cargo` + `rustc`; `node`
        /// for both `ts` and `js`) and nothing for the four self-contained
        /// frontends; the error names the language, the missing tool, and its
        /// install hint — no panic, no I/O.
        #[test]
        fn scan_time_tools_and_error_text() {
            assert_eq!(scan_time_tools("go"), &["go"][..], "go needs the go tool");
            assert_eq!(scan_time_tools("java"), &["java"][..], "java needs a JDK");
            assert_eq!(
                scan_time_tools("rust"),
                &["cargo", "rustc"][..],
                "rust needs cargo + rustc"
            );
            assert_eq!(scan_time_tools("ts"), &["node"][..], "ts needs node");
            assert_eq!(scan_time_tools("js"), &["node"][..], "js needs node");
            for lang in ["cpp", "csharp", "py", "md"] {
                assert!(
                    scan_time_tools(lang).is_empty(),
                    "{lang} is self-contained at scan time"
                );
            }
            assert!(
                scan_time_tools("unknown-lang").is_empty(),
                "an unknown language never demands a tool"
            );

            let go = scan_time_tool_error("go", "go");
            assert!(go.contains("`go` frontend"), "names the language: {go}");
            assert!(go.contains("required tool `go`"), "names the tool: {go}");
            assert!(go.contains("brew install go"), "carries the hint: {go}");

            let rust = scan_time_tool_error("rust", "cargo");
            assert!(rust.contains("`cargo`"), "names the tool: {rust}");
            assert!(
                rust.contains("brew install rust"),
                "carries the hint: {rust}"
            );

            let java = scan_time_tool_error("java", "java");
            assert!(java.contains("JDK"), "carries the JDK hint: {java}");

            let node = scan_time_tool_error("ts", "node");
            assert!(node.contains("`node`"), "names node: {node}");
            assert!(
                node.contains("brew install node"),
                "carries the hint: {node}"
            );
        }

        /// The discovered-work protocol: implementation-discovered work is
        /// re-planned (a planned node plus a `creates` task) before it is
        /// implemented. The embedded coordinator/authoring prompts must carry it
        /// so the rule cannot be silently dropped from the distributed agents.
        #[test]
        fn discovered_work_is_replanned_before_it_is_implemented() {
            fn agent(name: &str) -> &'static str {
                AGENTS
                    .iter()
                    .find(|(n, _)| *n == name)
                    .map(|(_, c)| *c)
                    .unwrap_or_else(|| panic!("{name} is in the embedded AGENTS set"))
            }
            assert!(
                agent("codebase-navigator.md")
                    .contains("discovered work is planned before it is implemented"),
                "the coordinator prompt must state the discovered-work order"
            );
            assert!(
                agent("agent-builder.md").contains("stops before editing"),
                "the agent-builder template must carry the stop-and-report clause"
            );
            assert!(
                agent("plan-writer.md").contains("declared before the code exists"),
                "the plan-writer prompt must carry the planned-before-code rule"
            );
            assert!(
                agent("spec-writer.md").contains("divergence discovered during implementation"),
                "the spec-writer prompt must carry the reconciliation route for discovered divergence"
            );
        }

        /// Phase-06 task-10: `builtin_code_type`'s JS rules are **extension-
        /// keyed, not stream-id-keyed** — the bundle-filename rule fires under
        /// BOTH the `ts` id (a mixed/TS-detected repo scans its incidental
        /// JavaScript under `ts`) and the `js` id. A bare `.cjs` suffix is never
        /// a bundle marker; only a real `min`/`bundle` filename infix (or a
        /// `gen|generated|dist|build|out` path segment) classifies generated.
        ///
        /// `classify_code_type` is metadata-only — it never drops a node — so
        /// every case additionally asserts the file stays a recognised in-graph
        /// code type (present, never filtered out).
        #[test]
        fn builtin_code_type_js_rules_are_extension_keyed() {
            let cases: &[(&str, &str)] = &[
                // Bundle filename markers: generated under both ids.
                ("/proj/src/app.min.js", "generated"),
                ("/proj/dist/app.min.js", "generated"),
                ("/proj/build/app.bundle.js", "generated"),
                ("/proj/src/app.min.jsx", "generated"),
                ("/proj/src/widget.min.mjs", "generated"),
                ("/proj/src/widget.bundle.mjs", "generated"),
                ("/proj/dist/app.min.cjs", "generated"),
                ("/proj/dist/app.bundle.cjs", "generated"),
                // A bare `.cjs`/`.js`/`.mjs` suffix is NEVER generated.
                ("/proj/src/index.js", "src"),
                ("/proj/src/util.mjs", "src"),
                ("/proj/src/index.cjs", "src"),
                ("/proj/src/x.cjs", "src"),
                ("/proj/src/widget.jsx", "src"),
                // test/vendor mirror the ts arm.
                ("/proj/tests/app.js", "test"),
                ("/proj/vendor/app.mjs", "external"),
            ];
            for (path, expected) in cases {
                for id in ["ts", "js"] {
                    let got = classify::classify_code_type(path, path, id, None);
                    assert_eq!(
                        got, *expected,
                        "{id} id: {path} must classify {expected}, got {got}"
                    );
                    assert!(
                        ["src", "test", "generated", "external"].contains(&got.as_str()),
                        "{id} id: {path} must stay an in-graph code type (never dropped), got {got}"
                    );
                }
            }
            // The `js` arm also recognises the JS filename test suffixes
            // (`*.test.js`/`*.spec.js` and the other JS extensions).
            for (path, expected) in [
                ("/proj/src/app.test.js", "test"),
                ("/proj/src/app.spec.mjs", "test"),
                ("/proj/src/app_test.cjs", "test"),
            ] {
                assert_eq!(
                    classify::classify_code_type(path, path, "js", None),
                    expected,
                    "js id: {path} must classify {expected}"
                );
            }
        }

        /// Phase-08 task-16: the `py` code-type arm. An ordinary module is
        /// `src`; a `*_test.py` (or `test_*.py`, or a `test/` segment) is
        /// `test`; a hand-written `.pyi` stub is `src` — NEVER `generated`
        /// (the `.d.ts` analog, a declaration is authored source) — UNLESS it
        /// sits under a `gen`/`generated` tree, which is `generated`; a
        /// `third_party/` file is `external`. `classify_code_type` is
        /// metadata-only — it never drops a node — so every case additionally
        /// asserts the file stays a recognised in-graph code type.
        #[test]
        fn builtin_code_type_py_rules_are_path_and_stem_keyed() {
            let cases: &[(&str, &str)] = &[
                ("/proj/pkg/mod.py", "src"),
                ("/proj/pkg/__init__.py", "src"),
                ("/proj/src/app.py", "src"),
                // Hand-written stubs are authored source, never generated.
                ("/proj/pkg/mod.pyi", "src"),
                ("/proj/stubs/pkg/mod.pyi", "src"),
                // A stub under a generated tree IS generated.
                ("/proj/gen/pkg/mod.pyi", "generated"),
                ("/proj/generated/pkg/mod.py", "generated"),
                // Test naming conventions and test trees.
                ("/proj/pkg/mod_test.py", "test"),
                ("/proj/test_helpers.py", "test"),
                ("/proj/tests/mod.py", "test"),
                // Dependency trees are external.
                ("/proj/third_party/lib/mod.py", "external"),
                ("/proj/vendor/lib/mod.py", "external"),
            ];
            for (path, expected) in cases {
                let got = classify::classify_code_type(path, path, "py", None);
                assert_eq!(got, *expected, "py id: {path} must classify {expected}");
                assert!(
                    ["src", "test", "generated", "external"].contains(&got.as_str()),
                    "py id: {path} must stay an in-graph code type (never dropped), got {got}"
                );
            }
        }
    }

    // -----------------------------------------------------------------------
    // Phase-06 unified JS/TS frontend acceptance helpers (tasks 12–15).
    // Non-#[test] helpers, so they live at the `mod tests` root.
    // -----------------------------------------------------------------------

    /// The staged unified JS/TS frontend artifact the candidate runs
    /// (`<profile>/frontends/tsfrontend/scanner.mjs` — the BUILT output staged
    /// by task-3). Fails loudly naming the build when absent.
    fn ts_frontend_artifact() -> PathBuf {
        let apg = crate::testutil::apg_bin();
        let p = apg
            .parent()
            .expect("apg binary has a parent")
            .join("frontends")
            .join("tsfrontend")
            .join("scanner.mjs");
        assert!(
            p.is_file(),
            "unified JS/TS frontend not found at {} — build it first: \
             cargo build --config 'env.APG_BUILD_FRONTENDS=\"ts\"'",
            p.display()
        );
        p
    }

    /// A scratch git repo for a JS/TS fixture: the caller's files, an isolated
    /// HOME with the suite dependency dir pre-created, and a committed
    /// `apg init` so the scan starts from a clean tree. Returns
    /// `(base, repo_dir, home)`.
    fn js_scratch_owned(tag: &str, files: &[(&str, String)]) -> (PathBuf, PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!("apg-js-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let repo_dir = base.join("repo");
        let home = base.join("home");
        std::fs::create_dir_all(home.join(".opencode/node_modules/@opencode-ai/plugin")).unwrap();
        scratch_repo_init(&repo_dir);
        for (rel, body) in files {
            let p = repo_dir.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        }
        scratch_commit_all(&repo_dir, "init source");
        let init = testutil::ApgCommand::new(&["init", "."])
            .cwd(&repo_dir)
            .env("HOME", home.to_str().unwrap())
            .output();
        assert!(
            init.status.success(),
            "apg init failed in {}: {}{}",
            repo_dir.display(),
            String::from_utf8_lossy(&init.stdout),
            String::from_utf8_lossy(&init.stderr)
        );
        scratch_commit_all(&repo_dir, "apg init");
        (base, repo_dir, home)
    }

    /// [`js_scratch_owned`] over borrowed string literals.
    fn js_scratch(tag: &str, files: &[(&str, &str)]) -> (PathBuf, PathBuf, PathBuf) {
        let owned: Vec<(&str, String)> =
            files.iter().map(|(r, b)| (*r, (*b).to_string())).collect();
        js_scratch_owned(tag, &owned)
    }

    /// The absolute source paths carried by `file` records in a scan export.
    fn export_file_paths(records: &[serde_json::Value]) -> BTreeSet<String> {
        records
            .iter()
            .filter(|r| r.get("type").and_then(|t| t.as_str()) == Some("file"))
            .filter_map(|r| r.get("fqn").and_then(|f| f.as_str()).map(str::to_string))
            .collect()
    }

    /// The module FQNs in a scan export.
    fn export_modules(records: &[serde_json::Value]) -> BTreeSet<String> {
        records
            .iter()
            .filter(|r| r.get("type").and_then(|t| t.as_str()) == Some("module"))
            .filter_map(|r| r.get("fqn").and_then(|f| f.as_str()).map(str::to_string))
            .collect()
    }

    /// The Struct/Function FQNs whose declared `path` is exactly `path`.
    fn export_symbols_at(records: &[serde_json::Value], path: &str) -> BTreeSet<String> {
        records
            .iter()
            .filter(|r| {
                matches!(
                    r.get("type").and_then(|t| t.as_str()),
                    Some("struct") | Some("function")
                )
            })
            .filter(|r| r.get("path").and_then(|p| p.as_str()) == Some(path))
            .filter_map(|r| r.get("fqn").and_then(|f| f.as_str()).map(str::to_string))
            .collect()
    }

    /// The resolve-only edge types (`calls`/`uses`/`unresolved_call`/
    /// `unresolved_use`) whose `from` FQN is one of `sources`.
    fn export_edge_types_from(
        records: &[serde_json::Value],
        sources: &BTreeSet<String>,
    ) -> BTreeSet<String> {
        records
            .iter()
            .filter(|r| {
                matches!(
                    r.get("type").and_then(|t| t.as_str()),
                    Some("calls") | Some("uses") | Some("unresolved_call") | Some("unresolved_use")
                )
            })
            .filter(|r| {
                r.get("from")
                    .and_then(|f| f.as_str())
                    .is_some_and(|f| sources.contains(f))
            })
            .filter_map(|r| r.get("type").and_then(|t| t.as_str()).map(str::to_string))
            .collect()
    }

    /// True when the export carries at least one `calls`/`uses` edge whose two
    /// endpoints are both declared project symbols (a resolve-only edge, never
    /// an unresolved target).
    fn has_resolved_project_edge(records: &[serde_json::Value]) -> bool {
        let project = export_symbol_fqns(records);
        records.iter().any(|r| {
            let ty = r.get("type").and_then(|t| t.as_str());
            if !matches!(ty, Some("calls") | Some("uses")) {
                return false;
            }
            let from = r.get("from").and_then(|f| f.as_str()).unwrap_or("");
            let to = r.get("to").and_then(|t| t.as_str()).unwrap_or("");
            project.contains(from) && project.contains(to)
        })
    }

    /// The `(fqn, category)` pairs of `unresolved` records.
    fn export_unresolved(records: &[serde_json::Value]) -> BTreeSet<(String, String)> {
        records
            .iter()
            .filter(|r| r.get("type").and_then(|t| t.as_str()) == Some("unresolved"))
            .map(|r| {
                (
                    r.get("fqn")
                        .and_then(|f| f.as_str())
                        .unwrap_or("")
                        .to_string(),
                    r.get("category")
                        .and_then(|c| c.as_str())
                        .unwrap_or("")
                        .to_string(),
                )
            })
            .collect()
    }

    /// The `calls`/`uses` edge targets in the export.
    fn export_resolved_edge_targets(records: &[serde_json::Value]) -> BTreeSet<String> {
        records
            .iter()
            .filter(|r| {
                matches!(
                    r.get("type").and_then(|t| t.as_str()),
                    Some("calls") | Some("uses")
                )
            })
            .filter_map(|r| r.get("to").and_then(|t| t.as_str()).map(str::to_string))
            .collect()
    }

    /// The first `file` record path ending with `suffix` — symlink-agnostic
    /// (`/var/...` vs `/private/var/...` on macOS) and identity-shape-agnostic:
    /// a `File` fqn is now the repo-relative identity (`calc.js`), so a
    /// leading-`/`suffix (`/calc.js`) matches either the relative fqn or an
    /// absolute path.
    fn export_file_ending(records: &[serde_json::Value], suffix: &str) -> Option<String> {
        let bare = suffix.trim_start_matches('/');
        export_file_paths(records)
            .into_iter()
            .find(|f| f.ends_with(suffix) || f.ends_with(bare))
    }

    /// Phase-06 task-12 fixture: a JS-only package with all four accepted JS
    /// extensions plus a `node_modules` dependency tree that must be skipped.
    fn js_only_fixture() -> Vec<(&'static str, &'static str)> {
        vec![
            (
                "calc.js",
                "function jsInner() { return 1; }\nfunction jsOuter() { return jsInner(); }\nmodule.exports = { jsOuter, jsInner };\n",
            ),
            (
                "widget.jsx",
                "function Widget() { return null; }\nfunction useWidget() { return Widget(); }\nfunction App() { return <Widget/>; }\nmodule.exports = { App, Widget, useWidget };\n",
            ),
            (
                "esm.mjs",
                "export function mjsInner() { return 2; }\nexport function mjsOuter() { return mjsInner(); }\n",
            ),
            (
                "cjs.cjs",
                "function loadDep() { return require('./dep.cjs'); }\nmodule.exports = { loadDep };\n",
            ),
            (
                "node_modules/leftpad/index.js",
                "function leftpadLeak() { return 3; }\nmodule.exports = { leftpadLeak };\n",
            ),
        ]
    }

    /// Phase-06 task-13 fixture: a mixed JS/TS package scanned ONCE under `ts`.
    /// Direction A: a `.js` file imports a `.ts` definition. Direction B: a
    /// `.ts` file imports a `.js` definition. Plus `package.json`
    /// `main`/`exports` self-name resolution, a JS workspace package resolved by
    /// name (the `workspaceHost` JS-candidate path), and `.d.ts` handling.
    fn mixed_js_ts_fixture() -> Vec<(&'static str, &'static str)> {
        vec![
            (
                "package.json",
                "{\n  \"name\": \"mixed\",\n  \"type\": \"module\",\n  \"workspaces\": [\"lib\"],\n  \"main\": \"./src/index.js\",\n  \"exports\": { \".\": \"./src/index.js\" }\n}\n",
            ),
            (
                "lib/package.json",
                "{\n  \"name\": \"mixed-lib\",\n  \"type\": \"module\",\n  \"main\": \"./index.js\"\n}\n",
            ),
            (
                "lib/index.js",
                "export function libHelper() { return 7; }\n",
            ),
            (
                "src/tsdef.ts",
                "export function tsHelper(): number { return 1; }\n",
            ),
            (
                "src/jsdef.js",
                "export function jsHelper(x) { return x + 1; }\n",
            ),
            ("src/types.d.ts", "export interface Config { x: number; }\n"),
            (
                "src/jsconsumer.js",
                "import { tsHelper } from \"./tsdef\";\nexport function useTs() { return tsHelper(); }\n",
            ),
            (
                "src/tsconsumer.ts",
                "import { jsHelper } from \"./jsdef\";\nimport type { Config } from \"./types\";\nexport function useJs(): number { return jsHelper(1); }\nexport function readConfig(c: Config): number { return c.x; }\n",
            ),
            (
                "src/index.js",
                "export function indexHelper() { return 42; }\n",
            ),
            (
                "src/selfconsumer.js",
                "import { indexHelper } from \"mixed\";\nimport { libHelper } from \"mixed-lib\";\nexport function useIndex() { return indexHelper(); }\nexport function useLib() { return libHelper(); }\n",
            ),
        ]
    }

    /// Phase-06 task-14 fixture: dynamic `require(expr)` and computed access
    /// only — the never-fabricate rule for untyped JavaScript.
    fn dynamic_js_fixture() -> Vec<(&'static str, &'static str)> {
        vec![(
            "dynamic.js",
            "function loadDynamic(name) {\n  const m = require(name);\n  return m();\n}\nfunction computed(obj, key) {\n  return obj[key]();\n}\nmodule.exports = { loadDynamic, computed };\n",
        )]
    }

    /// Phase-06 task-15 fixture: a COPY of the ported `src/tslib` —
    /// `scanner.ts` + its `package.json` (`name: apg-tsfrontend`) — as the scan
    /// target. Reading the real repo source is why this is e2e.
    fn tslib_self_scan_fixture() -> Vec<(&'static str, String)> {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let scanner = std::fs::read_to_string(root.join("src/tslib/scanner.ts"))
            .expect("read src/tslib/scanner.ts");
        let pkg = std::fs::read_to_string(root.join("src/tslib/package.json"))
            .expect("read src/tslib/package.json");
        vec![("tslib/scanner.ts", scanner), ("tslib/package.json", pkg)]
    }

    // -----------------------------------------------------------------------
    // Phase-08 Python frontend acceptance helpers (tasks 17–20). Non-#[test]
    // helpers, so they live at the `mod tests` root.
    // -----------------------------------------------------------------------

    /// The staged Python frontend the candidate runs
    /// (`<profile>/frontends/pyfrontend`), resolved from `testutil::apg_bin()`
    /// — the same artifact `apg scan` spawns. Fails loudly naming the build.
    fn py_frontend_bin() -> PathBuf {
        let apg = crate::testutil::apg_bin();
        let bin = apg
            .parent()
            .expect("apg binary has a parent")
            .join("frontends")
            .join("pyfrontend");
        assert!(
            bin.is_file(),
            "python frontend not found at {} — build it first: \
             cargo build --config 'env.APG_BUILD_FRONTENDS=\"py\"'",
            bin.display()
        );
        bin
    }

    /// Runs a scanner frontend binary over `dir` with `extra` argv and parses
    /// its emitted unified-schema records.
    fn frontend_stream(bin: &Path, dir: &Path, extra: &[&str]) -> Vec<crate::schema::Record> {
        let out = std::process::Command::new(bin)
            .arg(dir)
            .args(extra)
            .output()
            .unwrap_or_else(|e| panic!("spawn {}: {e}", bin.display()));
        assert!(
            out.status.success(),
            "{} failed over {}: {}",
            bin.display(),
            dir.display(),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| {
                serde_json::from_str::<crate::schema::Record>(l)
                    .unwrap_or_else(|e| panic!("bad scanner record `{l}`: {e}"))
            })
            .collect()
    }

    /// Runs the Python frontend directly over `repo_dir` with `extra` argv.
    fn py_frontend_records_with(repo_dir: &Path, extra: &[&str]) -> Vec<crate::schema::Record> {
        frontend_stream(&py_frontend_bin(), repo_dir, extra)
    }

    /// [`py_frontend_records_with`] with no extra argv.
    fn py_frontend_records(repo_dir: &Path) -> Vec<crate::schema::Record> {
        py_frontend_records_with(repo_dir, &[])
    }

    /// A scratch git repo for a Python fixture: the caller's files, an isolated
    /// HOME (the suite dependency dir pre-created so `apg init` never shells
    /// out to npm), a committed `apg init`, and both commits. Returns
    /// `(base, repo_dir, home)`.
    fn py_scratch(tag: &str, files: &[(&str, &str)]) -> (PathBuf, PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!("apg-py-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let repo_dir = base.join("repo");
        let home = base.join("home");
        std::fs::create_dir_all(home.join(".opencode/node_modules/@opencode-ai/plugin")).unwrap();
        scratch_repo_init(&repo_dir);
        for (rel, body) in files {
            let p = repo_dir.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        }
        scratch_commit_all(&repo_dir, "init source");
        let init = testutil::ApgCommand::new(&["init", "."])
            .cwd(&repo_dir)
            .env("HOME", home.to_str().unwrap())
            .output();
        assert!(
            init.status.success(),
            "apg init failed in {}: {}{}",
            repo_dir.display(),
            String::from_utf8_lossy(&init.stdout),
            String::from_utf8_lossy(&init.stderr)
        );
        scratch_commit_all(&repo_dir, "apg init");
        (base, repo_dir, home)
    }

    /// A Python scratch repo that has NOT yet been `apg init`'d (the caller
    /// controls the init/commit sequence for the incremental scenarios).
    /// Returns `(base, repo_dir)`.
    fn py_incremental_scratch(tag: &str, files: &[(&str, &str)]) -> (PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!("apg-pywinb-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let repo_dir = base.join("repo");
        let home = base.join("home");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(home.join(".opencode/node_modules/@opencode-ai/plugin")).unwrap();
        scratch_repo_init(&repo_dir);
        for (rel, body) in files {
            let p = repo_dir.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        }
        scratch_commit_all(&repo_dir, "init source");
        (base, repo_dir)
    }

    /// P9 roots a module/symbol FQN under a `<lang>.` segment; strip a leading
    /// Python root (`py.`/`python.`) so a module IDENTITY assertion holds both
    /// pre-rooting (bare `pkg`) and post-rooting (`py.pkg`). Only the Python
    /// fixtures (modules `pkg`/`pkg.sub`/`foo`/`bar`) are passed through it, so
    /// a real module named after a language token is never rewritten.
    fn strip_lang_prefix(fqn: &str) -> String {
        match fqn.split_once('.') {
            Some(("py" | "python", rest)) => rest.to_string(),
            _ => fqn.to_string(),
        }
    }

    /// Writes a real `apg/config.json` opting this fixture into the py
    /// provenance classes. `apg init` writes a `default: src` layout whose empty
    /// `types` REPLACES the builtin rules; this keeps the version field. The
    /// builtin py arm is asserted separately on the frontend's own records.
    fn write_py_config(repo_dir: &Path) {
        std::fs::write(
            repo_dir.join("apg/config.json"),
            format!(
                "{{\n  \"default\": \"src\",\n  \"types\": [\n    \
                 {{ \"name\": \"test\", \"globs\": [\"**/*_test.py\", \"**/test_*.py\", \"**/tests/**\"] }},\n    \
                 {{ \"name\": \"generated\", \"globs\": [\"**/gen/**\", \"**/generated/**\"] }},\n    \
                 {{ \"name\": \"external\", \"globs\": [\"**/third_party/**\", \"**/vendor/**\"] }}\n  \
                 ],\n  \"version\": \"{}\"\n}}\n",
                env!("CARGO_PKG_VERSION")
            ),
        )
        .unwrap();
    }

    /// Phase-08 task-17 fixture: a Python repo carrying the pinned acceptance
    /// shapes — a package + sub-package, distinct-stem flat modules, a `.pyi`
    /// stub, a `*_test.py`, a `third_party/` file, a `site-packages/` tree
    /// (must yield NO code node) and a `.pyx` (never scanned).
    fn py_acceptance_fixture() -> Vec<(&'static str, &'static str)> {
        vec![
            ("pkg/__init__.py", ""),
            ("pkg/sub/__init__.py", ""),
            ("pkg/sub/mod.py", "def sub_fn() -> int:\n    return 1\n"),
            ("pkg/sub/api.pyi", "def typed(x: int) -> int: ...\n"),
            ("pkg/lifecycle_test.py", "def check() -> None:\n    pass\n"),
            ("foo.py", "def foo() -> int:\n    return 1\n"),
            ("bar.py", "def bar() -> int:\n    return 2\n"),
            (
                "third_party/lib/dep.py",
                "def dep() -> int:\n    return 3\n",
            ),
            (
                "site-packages/vendored/dep.py",
                "def hidden() -> int:\n    return 4\n",
            ),
            ("cy/mod.pyx", "def cyfn():\n    pass\n"),
        ]
    }

    /// Phase-08 task-18 fixture: cross-file calls/uses, a project-class
    /// constructor call, a stdlib reference (`os.getcwd`), a hand-built `.venv`
    /// dependency (markers + stub site-packages, no interpreter) and in-root
    /// dynamic/unbound references (a call through a module-level callable alias
    /// and a base class that is an in-root alias binding — ty resolves both to a
    /// project-file position that is not a declaration).
    fn py_resolution_fixture() -> Vec<(&'static str, &'static str)> {
        vec![
            (
                "pyproject.toml",
                "[project]\nname = \"probe\"\nversion = \"0.0.0\"\n",
            ),
            ("pkg/__init__.py", ""),
            ("pkg/base.py", "class Base:\n    pass\n"),
            (
                "pkg/aliases.py",
                "from pkg.base import Base\n\n\nBaseAlias = Base\n",
            ),
            (
                "pkg/b.py",
                "def helper() -> int:\n    return 1\n\n\nhelper_alias = helper\n",
            ),
            (
                "pkg/a.py",
                "from pkg.aliases import BaseAlias\nfrom pkg.b import helper, helper_alias\n\
                 from pkg.base import Base\n\n\n\
                 class User(Base):\n    pass\n\n\n\
                 class Aliased(BaseAlias):\n    pass\n\n\n\
                 def call() -> int:\n    return helper()\n\n\n\
                 def call_aliased() -> int:\n    return helper_alias()\n\n\n\
                 def make() -> Base:\n    return Base()\n",
            ),
            (
                "stdlib_ref.py",
                "import os\n\n\ndef where() -> str:\n    return os.getcwd()\n",
            ),
            (
                ".venv/pyvenv.cfg",
                "home = /nonexistent\nversion = 3.12.0\n",
            ),
            (
                ".venv/lib/python3.12/site-packages/dep/__init__.py",
                "def thing() -> int:\n    return 1\n",
            ),
            (
                "external_ref.py",
                "import dep\n\n\ndef use() -> int:\n    return dep.thing()\n",
            ),
        ]
    }

    /// Phase-08 task-19 fixture: `pkg.a` calls `pkg.b.helper` — the unchanged,
    /// un-emitted target of the targeted re-scan scenario.
    fn py_incremental_fixture() -> Vec<(&'static str, &'static str)> {
        vec![
            ("pkg/__init__.py", ""),
            (
                "pkg/a.py",
                "from pkg.b import helper\n\n\ndef call() -> int:\n    return helper()\n",
            ),
            ("pkg/b.py", "def helper() -> int:\n    return 1\n"),
        ]
    }

    /// True when the export carries a resolve-only edge `from --kind--> to`,
    /// ROOTING-AGNOSTICALLY (a leading `py.`/`python.` root is normalised away
    /// on both endpoints, so the assertion holds pre- and post-P9).
    fn has_edge_between(records: &[serde_json::Value], kind: &str, from: &str, to: &str) -> bool {
        records.iter().any(|r| {
            r.get("type").and_then(|t| t.as_str()) == Some(kind)
                && r.get("from")
                    .and_then(|f| f.as_str())
                    .is_some_and(|f| strip_lang_prefix(f) == from)
                && r.get("to")
                    .and_then(|t| t.as_str())
                    .is_some_and(|t| strip_lang_prefix(t) == to)
        })
    }

    /// e2e tier -- real I/O: these tests read repo files (README/Cargo.toml/
    /// suite sources), build scratch git repos, drive the candidate `apg`
    /// binary or open `db.lbug`. Each is `#[ignore]`d, so a plain `cargo test`
    /// never runs one; the only entry point is the named guard `cargo test-e2e`
    /// (= `cargo test tests::e2e:: -- --ignored`).
    mod e2e {
        use super::*;

        /// Phase-09 task-41 (COORDINATOR RE-SCOPE: the scratch-repo / in-place
        /// drift oracle is dropped). An in-process test asserts the
        /// implemented-by migration invariant through the ACTUAL rooting code:
        /// every bare migration source FQN the phase re-points renders as its
        /// rooted destination, so no authored `implemented-by` target can remain
        /// un-rooted. `ingest` spools to a temp file, so this is e2e by the tier
        /// law and is `#[ignore]`d. It reads NO `apg/layers` node files and no
        /// `db.lbug`.
        #[test]
        #[ignore = "e2e tier: real I/O (temp spool dir); run via cargo test-e2e"]
        fn rooting_renders_every_implemented_by_migration_target() {
            // (language, bare migration source FQN, rooted destination FQN).
            // The first six are the coordinator's named cross-language cases;
            // the rest are the `apg.*` -> `rust.apg.*` family (a representative
            // spread across the migrated owners).
            let cases: &[(&str, &str, &str)] = &[
                ("rust", "apg.cmd_scan", "rust.apg.cmd_scan"),
                (
                    "rust",
                    "build_script_build.main",
                    "rust.build_script_build.main",
                ),
                (
                    "java",
                    "CallGraphBuilder.main",
                    "java.CallGraphBuilder.main",
                ),
                ("go", "apg/gofrontend.main", "go.apg/gofrontend.main"),
                (
                    "csharp",
                    "Apg.CsharpFrontend.Program.Main",
                    "csharp.Apg.CsharpFrontend.Program.Main",
                ),
                ("cpp", "cpplib.main", "cpp.cpplib.main"),
                ("rust", "apg.main", "rust.apg.main"),
                ("rust", "apg.scanner_records", "rust.apg.scanner_records"),
                ("rust", "apg.ingest.claim", "rust.apg.ingest.claim"),
                ("rust", "apg.ingest.ingest", "rust.apg.ingest.ingest"),
                (
                    "rust",
                    "apg.ingest.render_function_fqns",
                    "rust.apg.ingest.render_function_fqns",
                ),
                (
                    "rust",
                    "apg.load.build_load_files",
                    "rust.apg.load.build_load_files",
                ),
                ("rust", "apg.load.copy_from", "rust.apg.load.copy_from"),
                (
                    "rust",
                    "apg.load.create_schema",
                    "rust.apg.load.create_schema",
                ),
                (
                    "rust",
                    "apg.load.write_graph_jsonl",
                    "rust.apg.load.write_graph_jsonl",
                ),
                (
                    "rust",
                    "apg.auto_detect_languages",
                    "rust.apg.auto_detect_languages",
                ),
                ("rust", "apg.frontend_cmd", "rust.apg.frontend_cmd"),
                ("rust", "apg.has_extension", "rust.apg.has_extension"),
                ("rust", "apg.id_prefix_for", "rust.apg.id_prefix_for"),
                (
                    "rust",
                    "apg.classify.builtin_code_type",
                    "rust.apg.classify.builtin_code_type",
                ),
                (
                    "rust",
                    "apg.classify.classify_code_type",
                    "rust.apg.classify.classify_code_type",
                ),
                ("rust", "apg.git.git_state", "rust.apg.git.git_state"),
                ("rust", "apg.git.is_stale", "rust.apg.git.is_stale"),
                (
                    "rust",
                    "apg.git.reanchor_scan_meta",
                    "rust.apg.git.reanchor_scan_meta",
                ),
                (
                    "rust",
                    "apg.git.recorded_scan",
                    "rust.apg.git.recorded_scan",
                ),
                (
                    "rust",
                    "apg.schema.Record.ScanMeta",
                    "rust.apg.schema.Record.ScanMeta",
                ),
                (
                    "rust",
                    "apg.session.Coordinator.start",
                    "rust.apg.session.Coordinator.start",
                ),
            ];
            let function = |id: &str, parent: &str, name: &str| crate::schema::Record::Function {
                id: id.to_string(),
                parent: parent.to_string(),
                name: name.to_string(),
                params: vec![],
                file: "/abs/x".to_string(),
                path: "/abs/x".to_string(),
                start: 0,
                end: 1,
                start_line: 1,
                end_line: 1,
            };
            for &(language, bare, rooted) in cases {
                let (parent, name) = bare
                    .rsplit_once('.')
                    .unwrap_or_else(|| panic!("`{bare}` must carry a parent scope"));
                let records: Vec<crate::schema::Record> = vec![
                    crate::schema::Record::LangSwitch {
                        language: language.to_string(),
                    },
                    function("n1", parent, name),
                ];
                let (graph, report) = crate::ingest::ingest(
                    records,
                    &crate::ingest::IngestOptions {
                        blacklist: &[],
                        language,
                        config: None,
                        base: None,
                    },
                );
                assert_eq!(report.shadowed_modules, 0, "`{bare}` must not shadow");
                assert_eq!(report.shadowed_functions, 0, "`{bare}` must not shadow");
                assert!(
                    graph.nodes.contains_key(rooted),
                    "`{bare}` must render as the rooted `{rooted}` — the \
                     implemented-by migration target"
                );
                assert!(
                    !graph.nodes.contains_key(bare),
                    "the bare migration target `{bare}` must no longer resolve"
                );
            }
        }

        /// Phase-06 task-11: `auto_detect_languages` — a JS-only tree detects
        /// `js`; a tree with `.ts` plus incidental `.js` detects `ts` ONCE (js
        /// suppressed, never js+ts as two frontends); a `node_modules`-only
        /// JavaScript tree never triggers `js` (the walk skips dependency dirs).
        #[test]
        #[ignore = "e2e tier: real I/O (temp dir fs); run via cargo test-e2e"]
        fn auto_detect_languages_js_and_ts_suppression() {
            let base = std::env::temp_dir().join(format!("apg-js-detect-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&base);
            let avail = vec!["ts".to_string(), "js".to_string()];

            // JS-only: js detected, ts not.
            std::fs::create_dir_all(base.join("jso")).unwrap();
            std::fs::write(base.join("jso/app.js"), "export const x = 1;\n").unwrap();
            assert_eq!(
                auto_detect_languages(&base.join("jso"), &avail),
                vec!["js".to_string()],
                "a JS-only tree detects js"
            );

            // .ts + incidental .js (and all four JS extensions): ts once, no js.
            std::fs::create_dir_all(base.join("mixed/src")).unwrap();
            std::fs::write(base.join("mixed/src/a.ts"), "export const a = 1;\n").unwrap();
            std::fs::write(base.join("mixed/src/b.js"), "export const b = 1;\n").unwrap();
            std::fs::write(base.join("mixed/src/c.jsx"), "export const c = 1;\n").unwrap();
            std::fs::write(base.join("mixed/src/d.mjs"), "export const d = 1;\n").unwrap();
            std::fs::write(base.join("mixed/src/e.cjs"), "module.exports = 1;\n").unwrap();
            assert_eq!(
                auto_detect_languages(&base.join("mixed"), &avail),
                vec!["ts".to_string()],
                ".ts + incidental .js detects ts once (js suppressed)"
            );

            // node_modules-only JavaScript: no js (dependency dirs are skipped).
            std::fs::create_dir_all(base.join("nm/node_modules/dep")).unwrap();
            std::fs::write(
                base.join("nm/node_modules/dep/index.js"),
                "module.exports = 1;\n",
            )
            .unwrap();
            assert!(
                auto_detect_languages(&base.join("nm"), &avail).is_empty(),
                "a node_modules-only JS tree never triggers js detection"
            );

            let _ = std::fs::remove_dir_all(&base);
        }

        #[test]
        #[ignore = "e2e tier: real I/O (repo files/scratch repo/spawned apg/db.lbug); run via cargo test-e2e"]
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
            let db =
                Database::new(dir.join(specs::TRANS).join("db.lbug"), Default::default()).unwrap();
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
        #[ignore = "e2e tier: real I/O (repo files/scratch repo/spawned apg/db.lbug); run via cargo test-e2e"]
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
        #[ignore = "e2e tier: real I/O (repo files/scratch repo/spawned apg/db.lbug); run via cargo test-e2e"]
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
        #[ignore = "e2e tier: real I/O (repo files/scratch repo/spawned apg/db.lbug); run via cargo test-e2e"]
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
        #[ignore = "e2e tier: real I/O (repo files/scratch repo/spawned apg/db.lbug); run via cargo test-e2e"]
        fn suite_installs_node_edge_project_tools_and_retires_spec_invariant() {
            let dir =
                std::env::temp_dir().join(format!("apg-suite-install-{}", std::process::id()));
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
            let plan_add =
                std::fs::read_to_string(dir.join("tools").join("apg_plan_add.ts")).unwrap();
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
        #[ignore = "e2e tier: real I/O (repo files/scratch repo/spawned apg/db.lbug); run via cargo test-e2e"]
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

        /// The graph-first ordering is stated on the installed navigator prompt
        /// and carried into `AGENTS.md`: for ANY code or structure question —
        /// discovery and enumeration included — the first tool call is a graph
        /// query, and `read`/`grep`/`glob` confirm and anchor a graph result or
        /// read artifacts the graph does not model; they never discover a graph
        /// fact. The navigator's rule names both artifact classes; 0.17.0 SHRINKS
        /// the not-in-graph class to "no symbols, not no node" — Ruby sources
        /// (`apg-ruby` is pending) and binaries have no frontend, so they appear
        /// only as residual `misc` `File` nodes with no Module/Struct/Function
        /// facts, and only the config-scope / standard-exclusion paths have no
        /// node at all; the in-graph class names the tracked
        /// text/config/packaging files the bundled structural scanner claims.
        /// `agent-builder.md` rule 5 makes every generated agent inherit the
        /// ordering aligned to that shrunk boundary.
        #[test]
        #[ignore = "e2e tier: real I/O (repo files/scratch repo/spawned apg/db.lbug); run via cargo test-e2e"]
        fn graph_first_rule_stated_in_navigator_prompt_and_guide() {
            let navigator = AGENTS
                .iter()
                .find(|(n, _)| *n == "codebase-navigator.md")
                .map(|(_, c)| *c)
                .unwrap_or_else(|| panic!("codebase-navigator.md is in the embedded AGENTS set"));
            let agents_md = include_str!("../AGENTS.md");

            // The navigator states the ordering as a non-negotiable rule: query
            // first for ANY code/structure question (discovery included), and
            // file tools confirm/anchor or read non-modelled artifacts.
            assert!(
                navigator.contains("Graph first, then file read"),
                "the navigator prompt must state the graph-first, then-file-read rule"
            );
            for needle in ["do not discover graph facts", "confirm and anchor"] {
                assert!(
                    navigator.contains(needle),
                    "the navigator prompt must state `{needle}` in the ordering rule"
                );
            }

            // The rule names both artifact classes. The not-in-graph header now
            // carries the corrected boundary inline — `Not in the graph (no
            // symbols, not no node):` — so pin the parenthetical header, not the
            // bare `Not in the graph:` prefix the reword no longer contains.
            for needle in [
                "In the graph:",
                "Not in the graph (no symbols, not no node):",
            ] {
                assert!(
                    navigator.contains(needle),
                    "the navigator prompt's rule must name the `{needle}` artifact class"
                );
            }

            // 0.17.0 SHRINKS the boundary. The in-graph class must name the
            // tracked text/config/packaging files the bundled structural scanner
            // claims (no longer a "not in the graph" class).
            for needle in [
                "text/config/packaging",
                "Cargo.toml",
                "install.sh",
                "release.yml",
            ] {
                assert!(
                    navigator.contains(needle),
                    "the navigator's in-graph class must name `{needle}` — a tracked \
                     text/config/packaging file the structural scanner claims"
                );
            }
            // …and the not-in-graph class shrinks to Ruby sources (no frontend
            // yet, `apg-ruby` pending), binaries and config-excluded paths.
            for needle in ["Ruby", "apg-ruby", "binaries", "config scope"] {
                assert!(
                    navigator.contains(needle),
                    "the navigator's shrunk not-in-graph class must name `{needle}`"
                );
            }
            // The corrected boundary is "no symbols, not no node": Ruby sources
            // and binaries have no frontend, so they appear only as residual
            // `misc` File nodes with no Module/Struct/Function facts; only
            // config-scope / standard-exclusion paths have no node at all.
            for needle in ["no symbols", "misc"] {
                assert!(
                    navigator.contains(needle),
                    "the navigator's not-in-graph class must convey `{needle}` — the \
                     residual `misc` File-node mechanism for frontend-less sources"
                );
            }
            assert!(
                navigator.contains("no Module/Struct/Function facts"),
                "the navigator's not-in-graph class must state that the residual \
                 `misc` File nodes carry no Module/Struct/Function facts"
            );

            // AGENTS.md carries the same qualifier in the file-tool guidance
            // ("Other tools" and the "Read those files with read, grep, or bash"
            // line): file tools confirm/anchor graph results or read non-modelled
            // artifacts; they never discover a graph fact.
            for needle in ["confirm and anchor", "do not discover graph facts"] {
                assert!(
                    agents_md.contains(needle),
                    "AGENTS.md must carry the graph-first qualifier `{needle}`"
                );
            }

            // agent-builder.md rule 5 makes every generated agent inherit the
            // ordering, not only the pre-existing navigator rules.
            let builder = AGENTS
                .iter()
                .find(|(n, _)| *n == "agent-builder.md")
                .map(|(_, c)| *c)
                .unwrap();
            assert!(
                builder.contains("graph-first, then-file-read ordering"),
                "agent-builder.md rule 5 must require generated agents to inherit the graph-first ordering"
            );
            // …aligned to the shrunk 0.17.0 not-in-graph boundary, so a generated
            // agent inherits the same artifact-class list.
            for needle in ["Ruby", "apg-ruby", "binaries"] {
                assert!(
                    builder.contains(needle),
                    "agent-builder.md rule 5 must align generated agents to the \
                     shrunk not-in-graph boundary via `{needle}`"
                );
            }
            assert!(
                builder.contains("no symbols"),
                "agent-builder.md rule 5 must carry the same \"no symbols\" sense of \
                 the shrunk not-in-graph boundary"
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
        #[ignore = "e2e tier: real I/O (repo files/scratch repo/spawned apg/db.lbug); run via cargo test-e2e"]
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

        /// The coordinator-mediated cycle's writer side is read-only: the embedded
        /// spec-writer and plan-writer prompts hold the read-only `apg_review`
        /// channel and no longer carry the `apg_review_action` literal anywhere
        /// (they read the transient store and return an ACTIONED/WONT-FIX claim;
        /// the coordinator actions the item), and the agent-builder template
        /// re-points the implementer and every test-implementer it scaffolds to
        /// that same read-only/claim-only grant shape.
        #[test]
        #[ignore = "e2e tier: real I/O (repo files/scratch repo/spawned apg/db.lbug); run via cargo test-e2e"]
        fn writer_and_test_implementer_agents_hold_read_only_feedback() {
            fn agent(name: &str) -> &'static str {
                AGENTS
                    .iter()
                    .find(|(n, _)| *n == name)
                    .map(|(_, c)| *c)
                    .unwrap_or_else(|| panic!("{name} is in the embedded AGENTS set"))
            }

            // The two writer prompts: the WHOLE embedded prompt text (not a single
            // frontmatter key) must be free of the action grant and must carry the
            // read-only review channel.
            for name in ["spec-writer.md", "plan-writer.md"] {
                let prompt = agent(name);
                assert!(
                    !prompt.contains("apg_review_action"),
                    "{name} must not carry the apg_review_action literal anywhere"
                );
                assert!(
                    prompt.contains("apg_review"),
                    "{name} must hold the read-only apg_review channel"
                );
            }

            // The agent-builder template: the implementer grant description and
            // every test-implementer grant description point at the read-only
            // channel and a claim, never the action tool.
            let builder = agent("agent-builder.md");
            fn section<'a>(doc: &'a str, heading: &str) -> &'a str {
                let start = doc.find(heading).unwrap_or_else(|| {
                    panic!("agent-builder.md must carry the `{heading}` heading")
                });
                let rest = &doc[start + heading.len()..];
                match rest.find("\n### ") {
                    Some(end) => &rest[..end],
                    None => rest,
                }
            }
            for heading in [
                "### <name>-implementer (one per detected subsystem; file `<name>-implementer.md`)",
                "### unit/int/e2e-test-implementer(s) (per detected tier, where a test tier is file-separable)",
            ] {
                let grant = section(builder, heading);
                assert!(
                    !grant.contains("apg_review_action"),
                    "{heading} must not grant apg_review_action"
                );
                assert!(
                    grant.contains("apg_review"),
                    "{heading} must grant the read-only apg_review channel"
                );
                assert!(
                    grant.contains("claim"),
                    "{heading} must return a claim rather than action the item"
                );
            }
        }

        #[test]
        #[ignore = "e2e tier: real I/O (repo files/scratch repo/spawned apg/db.lbug); run via cargo test-e2e"]
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
        #[ignore = "e2e tier: real I/O (repo files/scratch repo/spawned apg/db.lbug); run via cargo test-e2e"]
        fn scaffold_gitignore_accepts_existing_entries_in_either_spelling() {
            let d =
                std::env::temp_dir().join(format!("apg-gitignore-spell-{}", std::process::id()));
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
        #[ignore = "e2e tier: real I/O (repo files/scratch repo/spawned apg/db.lbug); run via cargo test-e2e"]
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

        #[test]
        #[ignore = "e2e tier: real I/O (repo files/scratch repo/spawned apg/db.lbug); run via cargo test-e2e"]
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
        #[ignore = "e2e tier: real I/O (repo files/scratch repo/spawned apg/db.lbug); run via cargo test-e2e"]
        fn readme_documents_release_version() {
            let readme =
                std::fs::read_to_string(format!("{}/README.md", env!("CARGO_MANIFEST_DIR")))
                    .unwrap();
            // The README pins the 0.17.x line, not an exact patch, so patch releases
            // don't require a README edit.
            assert!(readme.contains("apg 0.17.x"), "README --version examples");
            assert!(readme.contains("0.17.x"), "README tagged-release prose");
            assert!(
                readme.contains("--version 0.17.x"),
                "README Linux installer pin option"
            );
            // No stale release records: the previous versions must be fully replaced.
            assert!(!readme.contains("0.10"), "README must not reference 0.10");
            assert!(!readme.contains("0.11"), "README must not reference 0.11");
        }

        /// Phase-7 task-1 (E2E, top-level dispatch): the strict-mutation surface's
        /// refusal sweep. Every create arm — `node add`, `edge add`, `plan add`
        /// (the plan itself), and `plan add phase|task|planned` — refuses an
        /// existing entity (non-zero, error naming the `update`/`rm` follow-up, no
        /// store change); `rm` on an absent entity is non-zero, never a silent
        /// no-op.
        #[test]
        #[ignore = "e2e tier: real I/O (repo files/scratch repo/spawned apg/db.lbug); run via cargo test-e2e"]
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
                node_cmd::cmd_node(&argv(&["add", "requirements", "requirement", "r1"]))
                    .unwrap_err()
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
                node_cmd::cmd_node(&argv(&["rm", "requirements", "requirement", "ghost"]))
                    .unwrap_err()
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

        /// Phase-04 task-4 (acceptance): the `apg node` / `apg edge` command
        /// surface is transparent — the SAME literal forms the CLI documents appear
        /// in `help_text`, the node/edge suite tools, and the distributed agent
        /// prompts.
        ///
        /// The expected strings are PINNED here as literals, not read back from the
        /// consts: a test that compares `AGENTS`/`SUITE_TOOLS` to themselves is a
        /// tautology and would pass even after a surface drift.
        #[test]
        #[ignore = "e2e tier: real I/O (repo files/scratch repo/spawned apg/db.lbug); run via cargo test-e2e"]
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
        #[ignore = "e2e tier: real I/O (repo files/scratch repo/spawned apg/db.lbug); run via cargo test-e2e"]
        fn read_access_during_a_live_session_routes_and_after_end_reads_directly() {
            let (repo, wt, wt_apg) = testutil::project_with_db("read-access");
            let home = repo.root.join("home");
            let session = testutil::start_session_process(&wt, &home);

            // A routed mutation lands and is projected write-through.
            let add =
                testutil::spawn_apg(&["node", "add", "requirements", "requirement", "live"], &wt);
            assert!(
                add.status.success(),
                "{}",
                String::from_utf8_lossy(&add.stderr)
            );

            // Routed read: post-mutation state, no lock error, no wait for end.
            let query =
                "MATCH (n:Requirement {fqn: 'requirements.requirement.live'}) RETURN count(n)";
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
        #[ignore = "e2e tier: real I/O (repo files/scratch repo/spawned apg/db.lbug); run via cargo test-e2e"]
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
                "go.fixture.mod.Store",
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
        #[ignore = "e2e tier: real I/O (repo files/scratch repo/spawned apg/db.lbug); run via cargo test-e2e"]
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
            let add =
                testutil::ApgCommand::new(&["node", "add", "requirements", "requirement", "foo"])
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
        #[ignore = "e2e tier: real I/O (repo files/scratch repo/spawned apg/db.lbug); run via cargo test-e2e"]
        fn acceptance_scratch_repo_real_init_scan_burst_read_your_writes_and_session() {
            let base =
                std::env::temp_dir().join(format!("apg-accept-scratch-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&base);
            let repo_dir = base.join("repo");
            let home = base.join("home");
            std::fs::create_dir_all(&home).unwrap();
            // Pre-create the opencode dependency dir so `apg init` never shells out
            // to npm (`cmd_init` skips npm when this path already exists) — keeps
            // the test hermetic and fast.
            std::fs::create_dir_all(home.join(".opencode/node_modules/@opencode-ai/plugin"))
                .unwrap();
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
            // branch DB (seeded by copying main's scan).
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

        /// Regression guard for the `apg query` failure-vs-data bug: a failed query
        /// came back from `runCypher` as an error *string*, the line-based
        /// `csvToRows` parsed it as data, and the plan tools then crashed with
        /// `undefined is not an object (evaluating 'fqn.replace')`.
        ///
        /// This is a STRUCTURAL check over the embedded suite consts (`APG_LIB` /
        /// `SUITE_TOOLS`), not full-file equality: it pins that the guard sits on
        /// the shared parse boundary (so a caller cannot forget it), that the
        /// producer (`runCypher`) and the discriminant (`isQueryError`) share the
        /// same prefix constants (so the failure signals cannot drift apart), and
        /// that the three observed crash sites route their parse through the guarded
        /// boundary and return the verbatim message. It fails on the pre-fix suite
        /// (where `csvToRows` split `out` directly and the guard was dead code).
        #[test]
        #[ignore = "e2e tier: real I/O (repo files/scratch repo/spawned apg/db.lbug); run via cargo test-e2e"]
        fn suite_tools_query_error_guard_is_structural() {
            // Extract a top-level function's source (signature through its closing
            // brace) from the embedded lib, so the assertions speak about the
            // function body rather than unrelated text elsewhere in the file.
            fn function_body<'a>(src: &'a str, signature: &str) -> &'a str {
                let start = src
                    .find(signature)
                    .unwrap_or_else(|| panic!("APG_LIB must declare `{signature}`"));
                // Skip the parameter list first: a braced option TYPE in the
                // signature (`opts?: { rebaseColumns?: number[] | ((row:
                // string[]) => number[]) }`) must not be mistaken for the
                // body's opening brace — its nested parens are counted too.
                let paren_open = src[start..]
                    .find('(')
                    .map(|i| start + i)
                    .unwrap_or_else(|| panic!("`{signature}` must have a parameter list"));
                let mut pdepth = 0i32;
                let mut paren_close = None;
                for (i, c) in src[paren_open..].char_indices() {
                    match c {
                        '(' => pdepth += 1,
                        ')' => {
                            pdepth -= 1;
                            if pdepth == 0 {
                                paren_close = Some(paren_open + i);
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                let paren_close = paren_close
                    .unwrap_or_else(|| panic!("`{signature}` has an unbalanced parameter list"));
                let open = src[paren_close..]
                    .find('{')
                    .map(|i| paren_close + i)
                    .unwrap_or_else(|| panic!("`{signature}` must have a body"));
                let mut depth = 0usize;
                for (i, c) in src[open..].char_indices() {
                    match c {
                        '{' => depth += 1,
                        '}' => {
                            depth -= 1;
                            if depth == 0 {
                                return &src[start..open + i + 1];
                            }
                        }
                        _ => {}
                    }
                }
                panic!("`{signature}` has an unbalanced body");
            }

            // 1. The guard is ON the shared parse boundary, by construction: the
            //    body of `csvToRows` — the one function every caller uses to turn a
            //    `runCypher` result into rows — invokes `expectQueryOk` before it
            //    splits any line. Pre-fix this body split `out` directly, leaving the
            //    guard as dead code a tool could forget; this is the assertion that
            //    fails on the old suite.
            let csv_body = function_body(APG_LIB, "export function csvToRows");
            assert!(
                csv_body.contains("expectQueryOk("),
                "csvToRows must call expectQueryOk at the shared parse boundary so a \
             runCypher error result can never be parsed as data: {csv_body}"
            );

            // 2. An error result cannot be mistaken for data: `runCypher` (the
            //    producer) and `isQueryError` (the discriminant) both build on the
            //    exported constants, so the prefixes cannot drift out of sync; and
            //    `runCypher` RETURNS the error string on the non-zero exit path —
            //    it never throws and never yields data.
            assert!(
                APG_LIB.contains("export const QUERY_FAILED_PREFIX")
                    && APG_LIB.contains("export const NO_DB_ERROR"),
                "runCypher's failure signals must be exported constants shared with isQueryError"
            );
            let run_body = function_body(APG_LIB, "export async function runCypher");
            let is_err_body = function_body(APG_LIB, "export function isQueryError");
            for (who, body) in [("runCypher", run_body), ("isQueryError", is_err_body)] {
                for const_name in ["QUERY_FAILED_PREFIX", "NO_DB_ERROR"] {
                    assert!(
                        body.contains(const_name),
                        "{who} must reference the shared `{const_name}` discriminant: {body}"
                    );
                }
            }
            let exit_check = run_body
                .find("result.exitCode !== 0")
                .expect("runCypher must classify success/failure by `apg query`'s exit code");
            assert!(
                !run_body
                    .lines()
                    .any(|l| l.trim_start().starts_with("throw")),
                "runCypher must RETURN the error string, never throw: {run_body}"
            );
            let failure = &run_body[exit_check..];
            let failure_return = failure
                .find("return")
                .expect("the non-zero exit path must return the error string");
            assert!(
                failure[failure_return..].contains("${QUERY_FAILED_PREFIX}"),
                "the failure return must carry the shared prefix so isQueryError \
             recognizes it: {}",
                &failure[failure_return..]
            );

            // 3. The three observed crash sites are covered: each embeds the shared
            //    guarded boundary and returns the verbatim error message, so a
            //    failure reads as a failure rather than as data (or an opaque crash).
            let tool = |name: &str| -> &'static str {
                SUITE_TOOLS
                    .iter()
                    .find(|(n, _)| *n == name)
                    .map(|(_, c)| *c)
                    .unwrap_or_else(|| panic!("SUITE_TOOLS must embed {name}"))
            };
            for name in ["apg_plan_tasks.ts", "apg_plan.ts", "apg_plan_phases.ts"] {
                let src = tool(name);
                assert!(
                    src.contains("../lib/apg.ts") && src.contains("csvToRows"),
                    "{name} must route its parse through the shared guarded boundary \
                 (csvToRows from ../lib/apg.ts)"
                );
                assert!(
                    src.contains("csvToRows(") && src.contains("await runCypher("),
                    "{name} must feed runCypher's result into the guarded csvToRows boundary"
                );
                assert!(
                    src.contains("try {"),
                    "{name} must wrap the guarded parse so a rejection is handled"
                );
                assert!(
                    src.contains("catch (e)")
                        && src.contains("e instanceof Error ? e.message : String(e)"),
                    "{name} must return the verbatim guarded-parse error message"
                );
            }
        }

        /// Phase-01 task-10 (int, scratch /tmp repo, CANDIDATE binary only —
        /// `global.constraint.no-real-project-test`): a second real `apg scan` of
        /// an unchanged repo takes the freshness fast-path — the verdict is
        /// printed, ZERO frontends run, and `db.lbug` is not rebuilt; a content
        /// edit falls back to a full re-scan. The printed `staleness_line` verdict
        /// agrees with the fast-path decision both ways: FRESH ⇒ fast-path, and a
        /// recorded-dirty tree whose content digest changed at the SAME sha prints
        /// STALE and falls through the full pipeline.
        #[test]
        #[ignore = "e2e tier: real I/O (repo files/scratch repo/spawned apg/db.lbug); run via cargo test-e2e"]
        fn acceptance_scan_freshness_fast_path_noop_and_content_edit_fallback() {
            let base = std::env::temp_dir().join(format!("apg-freshness-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&base);
            let repo_dir = base.join("repo");
            let home = base.join("home");
            std::fs::create_dir_all(&home).unwrap();
            // Keep `apg init` hermetic/fast: pre-create the opencode plugin dir so
            // it never shells out to npm.
            std::fs::create_dir_all(home.join(".opencode/node_modules/@opencode-ai/plugin"))
                .unwrap();
            let home_s = home.to_str().unwrap().to_string();

            scratch_repo_init(&repo_dir);
            std::fs::write(repo_dir.join("go.mod"), "module scratch\n\ngo 1.21\n").unwrap();
            std::fs::write(repo_dir.join("main.go"), "package main\n\nfunc main() {}\n").unwrap();
            scratch_commit_all(&repo_dir, "init source");

            let run_in = |dir: &Path, args: &[&str]| -> std::process::Output {
                let out = testutil::ApgCommand::new(args)
                    .cwd(dir)
                    .env("HOME", &home_s)
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
            let err_of =
                |out: &std::process::Output| String::from_utf8_lossy(&out.stderr).into_owned();

            // Real init + commit, then the first (cold) full scan.
            run_in(&repo_dir, &["init", "."]);
            scratch_commit_all(&repo_dir, "apg init");
            let first = run_in(&repo_dir, &["scan", "."]);
            assert!(
                err_of(&first).contains("[scan] running go frontend"),
                "the cold scan must run the go frontend: {}",
                err_of(&first)
            );
            let db = repo_dir.join("apg/.trans/db.lbug");
            let db_before = std::fs::read(&db).unwrap();

            // ---- (1) no-op re-scan: FRESH ⇒ fast-path, zero frontends, no rebuild.
            let second = run_in(&repo_dir, &["scan", "."]);
            let second_err = err_of(&second);
            assert!(
                second_err.contains("→ FRESH"),
                "the printed staleness verdict must be FRESH: {second_err}"
            );
            assert!(
                second_err.contains("fast-path"),
                "the fast-path verdict must be printed: {second_err}"
            );
            assert!(
                !second_err.contains("[scan] running"),
                "the fast-path must spawn no frontend: {second_err}"
            );
            assert_eq!(
                std::fs::read(&db).unwrap(),
                db_before,
                "the fast-path must not rebuild db.lbug"
            );

            // ---- (2) an uncommitted content edit at the same sha ⇒ STALE + full run.
            std::fs::write(
                repo_dir.join("main.go"),
                "package main\n\nfunc main() { helper() }\n\nfunc helper() {}\n",
            )
            .unwrap();
            let third = run_in(&repo_dir, &["scan", "."]);
            let third_err = err_of(&third);
            assert!(
                third_err.contains("→ STALE"),
                "an uncommitted content edit must print STALE: {third_err}"
            );
            assert!(
                !third_err.contains("fast-path"),
                "a stale tree must fall through to the full pipeline: {third_err}"
            );
            assert!(
                third_err.contains("[scan] running go frontend"),
                "a stale tree must run the frontend: {third_err}"
            );

            // ---- (3) recorded-dirty tree, digest changes at the SAME sha ⇒ STALE
            // from `staleness_line` and the full pipeline (never the fast-path).
            // The scan above recorded the dirty tree (main.go modified, uncommitted)
            // with its content digest; change the content again at the same sha.
            std::fs::write(
                repo_dir.join("main.go"),
                "package main\n\nfunc main() { helper(); helper() }\n\nfunc helper() {}\n",
            )
            .unwrap();
            let fourth = run_in(&repo_dir, &["scan", "."]);
            let fourth_err = err_of(&fourth);
            assert!(
                fourth_err.contains("→ STALE"),
                "a same-sha dirty-content change must print STALE: {fourth_err}"
            );
            assert!(
                !fourth_err.contains("fast-path"),
                "a same-sha dirty-content change must fall through: {fourth_err}"
            );
            assert!(
                fourth_err.contains("[scan] running go frontend"),
                "a same-sha dirty-content change must run the frontend: {fourth_err}"
            );

            // ---- teardown: the scratch repo AND the isolated HOME ----
            let _ = std::fs::remove_dir_all(&base);
        }

        // -------------------------------------------------------------------
        // Phase-02 win-B incremental integration (tasks 17 / 19). Every scenario
        // runs the CANDIDATE binary only, against a scratch /tmp git repo
        // (`global.constraint.no-real-project-test`).
        // -------------------------------------------------------------------

        /// Phase-02 task-9 (the pinned target-set hand-off contract, feedback-85):
        /// the `--targets <file>` list is newline-delimited absolute paths (blank
        /// lines ignored), and the same list is written per language from the
        /// checkout-relative target set. The channel is argv — stdin stays null.
        #[test]
        #[ignore = "e2e tier: real I/O (repo files/scratch repo/spawned apg/db.lbug); run via cargo test-e2e"]
        fn frontend_handoff_targets_file_is_newline_delimited_absolute_paths() {
            let tmp = std::env::temp_dir().join(format!("apg-handoff-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&tmp);
            std::fs::create_dir_all(&tmp).unwrap();

            let handoff = FrontendHandoff {
                targets_enabled: true,
                cache_dir: Some(PathBuf::from("/common/apg/facts")),
                cache_key: Some("cache-key-token".to_string()),
            };
            let targets = vec!["/root/b.go".to_string(), "/root/a.go".to_string()];
            let path = handoff.write_targets(&tmp, "go", &targets);
            let read = read_targets_file(&path);
            assert_eq!(read, targets, "the target list round-trips verbatim");

            // Blanks are ignored.
            std::fs::write(&path, "/root/x.go\n\n  \n/root/y.go\n").unwrap();
            assert_eq!(
                read_targets_file(&path),
                vec!["/root/x.go".to_string(), "/root/y.go".to_string()]
            );

            // An absent file parses to an empty list (no filter).
            assert!(read_targets_file(&tmp.join("missing.targets")).is_empty());

            // `targets_for_language` maps the checkout-relative set onto absolute
            // per-language paths under the scan root.
            let mut rel = BTreeSet::new();
            rel.insert("a/a.go".to_string());
            rel.insert("b/b.go".to_string());
            rel.insert("t/thing.ts".to_string());
            // A `.hxx` change is a C++ target (feedback-99): it must land in the
            // cpp list, not fall through to `other` and drop its facts.
            rel.insert("c/thing.hxx".to_string());
            let go = targets_for_language(&rel, Path::new("/root"), "go");
            assert_eq!(go, vec!["/root/a/a.go", "/root/b/b.go"]);
            let ts = targets_for_language(&rel, Path::new("/root"), "ts");
            assert_eq!(ts, vec!["/root/t/thing.ts"]);
            let cpp = targets_for_language(&rel, Path::new("/root"), "cpp");
            assert_eq!(cpp, vec!["/root/c/thing.hxx"]);

            // The pinned flags are the ONLY channel: a command built with the
            // hand-off carries `--targets`, `--cache-dir`, `--cache-key`.
            let mut cmd = Command::new("true");
            handoff.append(&mut cmd, &tmp, "go", &targets);
            let args: Vec<String> = cmd
                .get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect();
            assert_eq!(
                args,
                vec![
                    "--targets".to_string(),
                    path.display().to_string(),
                    "--cache-dir".to_string(),
                    "/common/apg/facts".to_string(),
                    "--cache-key".to_string(),
                    "cache-key-token".to_string(),
                ]
            );

            let _ = std::fs::remove_dir_all(&tmp);
        }

        /// Phase-02 task-17 (int): a targeted re-scan of each change class yields a
        /// graph exactly equal to a fresh full scan of the same tree — same node
        /// set, same edge set, same unresolved targets. The full-scan oracle runs
        /// with the shared fact store cleared (no reuse, no splice). Scratch /tmp
        /// repos, CANDIDATE binary only.
        #[test]
        #[ignore = "e2e tier: real I/O (repo files/scratch repo/spawned apg/db.lbug); run via cargo test-e2e"]
        #[allow(clippy::type_complexity)]
        fn acceptance_targeted_rescan_equivalence_leaf_body_signature_rename() {
            // Change classes. Go has no overloads, so the overload-peer rule is
            // covered by the impact unit test (task-16); the int-level classes are
            // leaf edit, body-only edit, signature change, and rename.
            let scenarios: &[(&str, fn(&Path))] = &[
                ("leaf-edit", |repo: &Path| {
                    // Add a declaration to the leaf file (its signature changes).
                    std::fs::write(
                    repo.join("a/a.go"),
                    "package a\n\n// A is a struct.\ntype A struct {\n\tX int\n}\n\n// Leaf is the leaf function.\nfunc Leaf() int { return 1 }\n\n// Extra is new.\nfunc Extra() int { return 2 }\n",
                )
                .unwrap();
                }),
                ("body-only-edit", |repo: &Path| {
                    // Change a function BODY without changing any declaration.
                    std::fs::write(
                    repo.join("a/a.go"),
                    "package a\n\n// A is a struct.\ntype A struct {\n\tX int\n}\n\n// Leaf is the leaf function.\nfunc Leaf() int { return 42 }\n",
                )
                .unwrap();
                }),
                ("signature-change", |repo: &Path| {
                    // Change the leaf's signature (params) — dependents cascade.
                    std::fs::write(
                    repo.join("a/a.go"),
                    "package a\n\n// A is a struct.\ntype A struct {\n\tX int\n}\n\n// Leaf takes a param now.\nfunc Leaf(n int) int { return n }\n",
                )
                .unwrap();
                }),
                ("rename", |repo: &Path| {
                    // Rename a file within its package (FQNs of its units persist;
                    // the File node path changes).
                    let from = repo.join("b/b.go");
                    let to = repo.join("b/bb.go");
                    let body = std::fs::read_to_string(&from).unwrap();
                    std::fs::remove_file(&from).unwrap();
                    std::fs::write(&to, body).unwrap();
                }),
            ];

            for (tag, mutate) in scenarios {
                let (base, repo_dir) = winb_scratch(tag, &winb_go_fixture());
                let home = base.join("home");
                let _ = winb_run(&repo_dir, &home, &["init", "."]);
                scratch_commit_all(&repo_dir, "apg init");
                // Cold scan: records the manifest + fact store.
                let cold = winb_run(&repo_dir, &home, &["scan", "."]);
                assert!(
                    cold.status.success(),
                    "{tag}: cold scan: {}",
                    String::from_utf8_lossy(&cold.stderr)
                );

                // Mutate the tree (working-tree change at the same sha).
                mutate(&repo_dir);

                // Incremental re-scan.
                let inc = winb_run(&repo_dir, &home, &["scan", "."]);
                assert!(
                    inc.status.success(),
                    "{tag}: incremental scan: {}",
                    String::from_utf8_lossy(&inc.stderr)
                );
                let inc_err = String::from_utf8_lossy(&inc.stderr);
                let (inc_nodes, inc_edges, inc_unres) = winb_graph(&repo_dir);

                // Oracle: a fresh FULL scan of the SAME tree with the fast-path,
                // the DB, AND the shared fact cache cleared — no reuse/splice.
                std::fs::remove_file(repo_dir.join("apg/.trans/db.lbug")).unwrap();
                std::fs::remove_file(repo_dir.join("apg/.trans/graph.jsonl")).unwrap();
                let store = repo_dir.join(".git/apg/facts");
                let _ = std::fs::remove_dir_all(&store);
                let full = winb_run(&repo_dir, &home, &["scan", "."]);
                assert!(
                    full.status.success(),
                    "{tag}: full scan oracle: {}",
                    String::from_utf8_lossy(&full.stderr)
                );
                let (full_nodes, full_edges, full_unres) = winb_graph(&repo_dir);

                assert_eq!(
                    inc_nodes, full_nodes,
                    "{tag}: node sets must be exactly equal (incremental vs full)\n{inc_err}"
                );
                assert_eq!(
                    inc_edges, full_edges,
                    "{tag}: edge sets must be exactly equal (incremental vs full)\n{inc_err}"
                );
                assert_eq!(
                    inc_unres, full_unres,
                    "{tag}: unresolved-target sets must be exactly equal\n{inc_err}"
                );

                // The incremental path must have taken the target-set hand-off (not
                // a silent full scan).
                assert!(
                    inc_err.contains("incremental:"),
                    "{tag}: the incremental verdict must be printed: {inc_err}"
                );

                // Signature early cutoff: a body-only change does NOT cascade to
                // dependents; a signature change DOES. `b` depends on `a` and `c` on
                // `b`, so the cascade marker's presence is a direct observable.
                let cascaded = inc_err.contains("signature change cascades");
                match *tag {
                    "body-only-edit" => assert!(
                        !cascaded,
                        "{tag}: a body-only change must not cascade: {inc_err}"
                    ),
                    "signature-change" => assert!(
                        cascaded,
                        "{tag}: a signature change must cascade: {inc_err}"
                    ),
                    _ => {}
                }

                let _ = std::fs::remove_dir_all(&base);
            }
        }

        /// Regression (reused-facts full-universe seam): a reused file whose
        /// cached facts are newer than THIS checkout's local export must still be
        /// in the code-FQN universe `ingest_tree` validates `implemented-by`
        /// against. Reproduces the multi-worktree lag (a shared-store record
        /// ahead of the local export) that made a merged code change look like
        /// `spec drift` on main's rebuild.
        #[test]
        #[ignore = "e2e tier: real I/O (scratch repo/spawned apg/db.lbug/fact store); run via cargo test-e2e"]
        fn acceptance_incremental_universe_includes_reused_facts_newer_than_local_export() {
            let (base, repo_dir) = winb_scratch("reuse-universe", &winb_go_fixture());
            let home = base.join("home");
            let _ = winb_run(&repo_dir, &home, &["init", "."]);
            scratch_commit_all(&repo_dir, "apg init");
            let cold = winb_run(&repo_dir, &home, &["scan", "."]);
            assert!(
                cold.status.success(),
                "cold scan: {}",
                String::from_utf8_lossy(&cold.stderr)
            );

            // Snapshot the L0 local export (its code universe has no `New`).
            let trans = repo_dir.join("apg/.trans");
            let db0 = std::fs::read(trans.join("db.lbug")).unwrap();
            let graph0 = std::fs::read(trans.join("graph.jsonl")).unwrap();

            // L1: add a new code unit and scan, so the SHARED store records its
            // FQN (and the local export advances too).
            std::fs::write(
                repo_dir.join("a/a.go"),
                "package a\n\ntype A struct {\n\tX int\n}\n\nfunc Leaf() int { return 1 }\n\nfunc New() int { return 2 }\n",
            )
            .unwrap();
            scratch_commit_all(&repo_dir, "add New");
            let l1 = winb_run(&repo_dir, &home, &["scan", "."]);
            assert!(
                l1.status.success(),
                "L1 scan: {}",
                String::from_utf8_lossy(&l1.stderr)
            );

            // The code FQN the store now carries but the L0 export does not.
            let graph = std::fs::read_to_string(trans.join("graph.jsonl")).unwrap();
            let new_fqn = graph
                .lines()
                .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
                .find_map(|v| {
                    let t = v.get("type").and_then(|t| t.as_str())?;
                    let fqn = v.get("fqn").and_then(|f| f.as_str())?;
                    (t == "function" && fqn.ends_with(".New")).then(|| fqn.to_string())
                })
                .expect("the L1 scan must carry the new function FQN");

            // L2: an authored `implemented-by` to `New` plus a non-code change
            // only, then rewind the LOCAL export to L0 (the shared store stays at
            // L1 — the multi-worktree lag).
            std::fs::create_dir_all(repo_dir.join("apg/layers/solution/component")).unwrap();
            std::fs::write(
                repo_dir.join("apg/layers/solution/component/scratch-seed.json"),
                format!(
                    "{{\n  \"layer\": \"solution\",\n  \"type\": \"component\",\n  \"name\": \"scratch-seed\",\n  \"body\": \"test component\",\n  \"properties\": {{}},\n  \"out\": [{{\"kind\": \"implemented-by\", \"target\": \"{new_fqn}\", \"properties\": {{}}}}],\n  \"in\": []\n}}\n"
                ),
            )
            .unwrap();
            std::fs::write(repo_dir.join("notes.txt"), "non-code change\n").unwrap();
            scratch_commit_all(&repo_dir, "authored component + note");
            std::fs::write(trans.join("db.lbug"), &db0).unwrap();
            std::fs::write(trans.join("graph.jsonl"), &graph0).unwrap();

            // The delta is non-code only, so `a/a.go` is REUSED from the store
            // (it carries `New`); without the reused-facts union the universe
            // would fall back to the L0 export and bail `spec drift`.
            let inc = winb_run(&repo_dir, &home, &["scan", "."]);
            let err = String::from_utf8_lossy(&inc.stderr);
            assert!(
                inc.status.success(),
                "the reused unit's new FQN must be in the universe: {err}"
            );
            assert!(!err.contains("spec drift"), "no false spec drift: {err}");

            let _ = std::fs::remove_dir_all(&base);
        }

        /// Phase-02 task-19 (int): cross-worktree cache sharing — the reuse half AND
        /// the exactness half. A cold full scan of a scratch repo records the
        /// reference graph; a SECOND clean near-identical worktree started from the
        /// same repo must (a) reuse the shared `<git-common-dir>/apg/facts` cache
        /// (no cold full frontend run) AND (b) produce a graph exactly equal to the
        /// full scan (same node set, edge set, unresolved targets). Candidate
        /// binary only, scratch /tmp repo.
        #[test]
        #[ignore = "e2e tier: real I/O (repo files/scratch repo/spawned apg/db.lbug); run via cargo test-e2e"]
        fn acceptance_cross_worktree_cache_sharing_exactness() {
            let (base, repo_dir) = winb_scratch("xwt", &winb_go_fixture());
            let home = base.join("home");
            let _ = winb_run(&repo_dir, &home, &["init", "."]);
            scratch_commit_all(&repo_dir, "apg init");

            // The cold full scan records the reference graph + the shared cache.
            let cold = winb_run(&repo_dir, &home, &["scan", "."]);
            assert!(
                cold.status.success(),
                "cold scan: {}",
                String::from_utf8_lossy(&cold.stderr)
            );
            let reference = std::fs::read(repo_dir.join("apg/.trans/graph.jsonl")).unwrap();
            let store = repo_dir.join(".git/apg/facts");
            assert!(store.is_dir(), "the shared store must exist after a scan");
            assert!(
                store.join("index.json").is_file(),
                "the shared store must index the per-file fact units"
            );

            // Start a SECOND clean worktree from the same repo (outside the main
            // checkout, so main stays clean). The worktree's committed source bytes
            // are identical to main's, so the relative-path-keyed units are
            // reusable. Raw git2 only — the candidate binary runs scans, nothing
            // else, and never against a real project.
            let wt = base.join("wt2");
            {
                let repo = git2::Repository::open(&repo_dir).unwrap();
                repo.worktree("wt2", &wt, None).unwrap();
                let wt_repo = git2::Repository::open(&wt).unwrap();
                wt_repo.set_head("refs/heads/wt2").unwrap();
                wt_repo
                    .checkout_head(Some(&mut git2::build::CheckoutBuilder::new().force()))
                    .unwrap();
            }
            // The worktree's own `apg/.trans/` marker (ignored content, so the
            // version gate + layout discovery resolve here, mirroring `project
            // start`).
            std::fs::create_dir_all(wt.join("apg/.trans")).unwrap();
            assert!(
                wt.join("apg/config.json").is_file(),
                "the committed layout config materializes in the worktree"
            );

            // The fresh worktree's scan reuses the shared store's units.
            let fresh = winb_run(&wt, &home, &["scan", "."]);
            assert!(
                fresh.status.success(),
                "fresh worktree scan: {}",
                String::from_utf8_lossy(&fresh.stderr)
            );
            let fresh_err = String::from_utf8_lossy(&fresh.stderr);

            // (a) REUSE: the fresh worktree took the incremental/reuse path against
            // the shared store (not a cold, cacheless full scan). At an unchanged
            // recorded HEAD the path is the strongest form — the warm-cache
            // assembly with zero frontend spawns.
            assert!(
                fresh_err.contains("incremental:")
                    || fresh_err.contains("reusable file")
                    || fresh_err.contains("warm cache"),
                "the fresh worktree must reuse the shared cache: {fresh_err}"
            );

            // (b) EXACTNESS: the fresh worktree's graph equals the full scan. File
            // node FQNs are absolute paths under the checkout root, so normalise
            // both sides to checkout-relative before comparing; every other record
            // must be exactly equal as a set.
            let norm_s = |root: &Path, set: BTreeSet<String>| -> BTreeSet<String> {
                let root_s = root.to_string_lossy().replace('\\', "/");
                set.into_iter()
                    .map(|s| s.replace(root_s.as_str(), "<root>"))
                    .collect()
            };
            let norm_e =
                |root: &Path, set: BTreeSet<(String, String)>| -> BTreeSet<(String, String)> {
                    let root_s = root.to_string_lossy().replace('\\', "/");
                    set.into_iter()
                        .map(|(t, e)| (t, e.replace(root_s.as_str(), "<root>")))
                        .collect()
                };
            let (f_nodes, f_edges, f_unres) = winb_graph(&wt);
            let (r_nodes, r_edges, r_unres) = winb_graph(&repo_dir);
            assert_eq!(
                norm_s(&wt, f_nodes),
                norm_s(&repo_dir, r_nodes),
                "cross-worktree node sets must be equal"
            );
            assert_eq!(
                norm_e(&wt, f_edges),
                norm_e(&repo_dir, r_edges),
                "cross-worktree edge sets must be equal"
            );
            assert_eq!(
                f_unres, r_unres,
                "cross-worktree unresolved sets must be equal"
            );
            let _ = &reference;

            let _ = std::fs::remove_dir_all(&base);
        }

        /// fix-module-identity task-17: a scratch /tmp repo's scans are
        /// checkout-independent — `apg scan <repo>` and `apg scan <repo>/subdir`
        /// mint the SAME repo-relative File identity for a shared file, main and
        /// its worktree agree at one commit, and raw `apg query` returns the
        /// stored form (never the absolute checkout path). Candidate binary
        /// only, scratch repo (`global.constraint.no-real-project-test`).
        #[test]
        #[ignore = "e2e tier: real I/O (scratch /tmp repo/fs/git/spawned apg); run via cargo test-e2e"]
        fn scan_is_checkout_independent_and_query_stays_stored() {
            let (base, repo_dir) = winb_scratch("relident", &winb_go_fixture());
            let home = base.join("home");
            let init = winb_run(&repo_dir, &home, &["init", "."]);
            assert!(
                init.status.success(),
                "init: {}",
                String::from_utf8_lossy(&init.stderr)
            );
            scratch_commit_all(&repo_dir, "apg init");

            let file_fqns = |repo: &Path| -> BTreeSet<String> {
                winb_graph(repo)
                    .0
                    .into_iter()
                    .filter_map(|n| n.strip_prefix("file:").map(str::to_string))
                    .collect()
            };

            // (1) Root scan: every File fqn is the repo-relative identity.
            let root_scan = winb_run(&repo_dir, &home, &["scan", "."]);
            assert!(
                root_scan.status.success(),
                "root scan: {}",
                String::from_utf8_lossy(&root_scan.stderr)
            );
            let root_files = file_fqns(&repo_dir);
            assert!(
                root_files.contains("a/a.go"),
                "relative identity: {root_files:?}"
            );
            let repo_abs = repo_dir.to_string_lossy().into_owned();
            for f in &root_files {
                assert!(!f.starts_with('/'), "no absolute File fqn: {f}");
                assert!(!f.contains(&repo_abs), "no checkout component: {f}");
            }

            // (2) Raw `apg query` is the graph view: the stored repo-relative
            // form, never the absolute path.
            let q = winb_run(
                &repo_dir,
                &home,
                &["query", "MATCH (f:File) RETURN f.fqn AS fqn ORDER BY f.fqn"],
            );
            assert!(
                q.status.success(),
                "query: {}",
                String::from_utf8_lossy(&q.stderr)
            );
            let qout = String::from_utf8_lossy(&q.stdout).into_owned();
            assert!(qout.contains("a/a.go"), "raw query stored form: {qout}");
            assert!(
                !qout.contains(&repo_abs),
                "raw query has no absolute path: {qout}"
            );

            // (3) Main == worktree at the same commit: identical identities.
            let wt = base.join("wt");
            {
                let repo = git2::Repository::open(&repo_dir).unwrap();
                repo.worktree("wt", &wt, None).unwrap();
                let wt_repo = git2::Repository::open(&wt).unwrap();
                wt_repo.set_head("refs/heads/wt").unwrap();
                wt_repo
                    .checkout_head(Some(&mut git2::build::CheckoutBuilder::new().force()))
                    .unwrap();
            }
            std::fs::create_dir_all(wt.join("apg/.trans")).unwrap();
            let wt_scan = winb_run(&wt, &home, &["scan", "."]);
            assert!(
                wt_scan.status.success(),
                "worktree scan: {}",
                String::from_utf8_lossy(&wt_scan.stderr)
            );
            assert_eq!(
                file_fqns(&wt),
                root_files,
                "main and worktree must mint the same File identities"
            );

            // (4) A SUBDIRECTORY scan mints the same identity for a shared file.
            let sub_scan = winb_run(&repo_dir, &home, &["scan", "b"]);
            assert!(
                sub_scan.status.success(),
                "subdir scan: {}",
                String::from_utf8_lossy(&sub_scan.stderr)
            );
            let sub_files = file_fqns(&repo_dir);
            assert!(
                sub_files.contains("b/b.go"),
                "subdir scan keeps the repo-relative identity: {sub_files:?}"
            );

            let _ = std::fs::remove_dir_all(&base);
        }

        /// fix-module-identity phase-03 task-7 (e2e): scan hygiene. A scratch
        /// /tmp git repo carries a Go test package (whose compile writes a
        /// generated `_testmain` into the shared Go build cache under
        /// `.git/apg/facts`), a `target/**` build tree (a `.ts` the unified
        /// frontend would otherwise scan, exactly like the observed
        /// `target/debug/frontends/tsfrontend/scanner.mjs`), and a gitignored
        /// build tree. After `apg scan`, ZERO File/Struct/Function record may
        /// carry a path under `.git/**` or `target/**` (or the gitignored
        /// tree), and no record may come from a generated `_testmain`. The
        /// same must hold for a linked worktree. Candidate binary only,
        /// scratch repo (`global.constraint.no-real-project-test`).
        #[test]
        #[ignore = "e2e tier: real I/O (scratch /tmp repo/git/spawned apg/db.lbug); run via cargo test-e2e"]
        fn scan_hygiene_excludes_git_target_and_gitignored_trees() {
            assert!(
                ts_frontend_artifact().is_file(),
                "the unified JS/TS frontend must be built for the target leak case"
            );
            let (base, repo_dir) = winb_scratch(
                "hygiene",
                &[
                    ("go.mod", "module scratch\n\ngo 1.21\n"),
                    (
                        "pkg/pkg.go",
                        "package pkg\n\n// Pkg is the package entry.\nfunc Pkg() int { return 1 }\n",
                    ),
                    (
                        "pkg/pkg_test.go",
                        "package pkg\n\nimport \"testing\"\n\nfunc TestPkg(t *testing.T) {\n\tif Pkg() != 1 {\n\t\tt.Fatal(\"pkg\")\n\t}\n}\n",
                    ),
                    // A TS source outside every build tree, so the unified
                    // frontend detects `ts` (the repo is go + ts).
                    (
                        "src/app.ts",
                        "export function app(): number { return 1; }\n",
                    ),
                    // A committed build-output tree the frontend would
                    // otherwise scan (the observed offender's shape).
                    (
                        "target/build/leak.ts",
                        "export function leakedFromTarget(): number { return 2; }\n",
                    ),
                    // A segment-exact control: `targets/` is NOT `target/**`,
                    // so it must survive the hygiene filter.
                    (
                        "targets/keep.ts",
                        "export function keptFromTargets(): number { return 4; }\n",
                    ),
                    // The gitignored build tree never enters the content
                    // identity, so it must not enter the graph either.
                    (".gitignore", "build-out/\n"),
                ],
            );
            let home = base.join("home");

            // A file under `.git/**` cannot be committed; mirror the per-scan
            // Go build-cache layout so a stray emission is caught.
            let cache_file = repo_dir.join(".git/apg/facts/go/key/53/deadbeef-d");
            std::fs::create_dir_all(cache_file.parent().unwrap()).unwrap();
            std::fs::write(&cache_file, "not source\n").unwrap();
            // The gitignored build tree: untracked, so git's ignore rules
            // (not the default `.git`/`target` predicate) must exclude it.
            let ignored = repo_dir.join("build-out/leak.ts");
            std::fs::create_dir_all(ignored.parent().unwrap()).unwrap();
            std::fs::write(
                &ignored,
                "export function leakedFromIgnored(): number { return 3; }\n",
            )
            .unwrap();

            let init = winb_run(&repo_dir, &home, &["init", "."]);
            assert!(
                init.status.success(),
                "init: {}",
                String::from_utf8_lossy(&init.stderr)
            );
            scratch_commit_all(&repo_dir, "apg init");

            // Independent (not the production predicate) segment check: a
            // record from a build-output / gitignored tree is a leak.
            let is_leak = |p: &str| {
                p.split(['/', '\\'])
                    .any(|s| s == ".git" || s == "target" || s == "build-out")
            };
            let assert_hygienic = |checkout: &Path, tag: &str| {
                let records = export_records(checkout);
                let files: BTreeSet<String> = records
                    .iter()
                    .filter(|r| r.get("type").and_then(|t| t.as_str()) == Some("file"))
                    .filter_map(|r| r.get("fqn").and_then(|f| f.as_str()).map(str::to_string))
                    .collect();
                // Never a vacuous pass: the real source of BOTH languages was
                // scanned.
                assert!(
                    files.contains("pkg/pkg.go"),
                    "{tag}: the Go package file must be scanned: {files:?}"
                );
                assert!(
                    files.contains("src/app.ts"),
                    "{tag}: the TS source file must be scanned: {files:?}"
                );
                assert!(
                    files.contains("targets/keep.ts"),
                    "{tag}: `targets/` is not `target/**` and must be scanned: {files:?}"
                );

                let mut offenders: Vec<String> = Vec::new();
                let mut testmain: Vec<String> = Vec::new();
                for r in &records {
                    let ty = r.get("type").and_then(|t| t.as_str()).unwrap_or("");
                    for key in ["fqn", "path"] {
                        let Some(v) = r.get(key).and_then(|x| x.as_str()) else {
                            continue;
                        };
                        if matches!(ty, "file" | "struct" | "function") && is_leak(v) {
                            offenders.push(format!("{ty}:{key}:{v}"));
                        }
                        if v.contains("_testmain") {
                            testmain.push(format!("{ty}:{key}:{v}"));
                        }
                    }
                }
                assert!(
                    offenders.is_empty(),
                    "{tag}: no File/Struct/Function may come from .git/**, target/** or a gitignored tree: {offenders:?}"
                );
                assert!(
                    testmain.is_empty(),
                    "{tag}: no record may come from a generated _testmain: {testmain:?}"
                );
            };

            // (1) The main checkout: the Go build cache lands under
            // `<repo>/.git/apg/facts`, inside the scanned tree.
            let scan = winb_run(&repo_dir, &home, &["scan", "."]);
            assert!(
                scan.status.success(),
                "main scan: {}",
                String::from_utf8_lossy(&scan.stderr)
            );
            let stderr = String::from_utf8_lossy(&scan.stderr);
            assert!(
                stderr.contains("Languages: go, ts") || stderr.contains("Languages: ts, go"),
                "the fixture must scan go + ts: {stderr}"
            );
            assert_hygienic(&repo_dir, "main");

            // (2) A linked worktree of the same repo: the committed target
            // tree materializes; the gitignored one is recreated untracked.
            let wt = base.join("wt");
            {
                let repo = git2::Repository::open(&repo_dir).unwrap();
                repo.worktree("wt", &wt, None).unwrap();
                let wt_repo = git2::Repository::open(&wt).unwrap();
                wt_repo.set_head("refs/heads/wt").unwrap();
                wt_repo
                    .checkout_head(Some(&mut git2::build::CheckoutBuilder::new().force()))
                    .unwrap();
            }
            std::fs::create_dir_all(wt.join("apg/.trans")).unwrap();
            let ignored_wt = wt.join("build-out/leak.ts");
            std::fs::create_dir_all(ignored_wt.parent().unwrap()).unwrap();
            std::fs::write(
                &ignored_wt,
                "export function leakedFromIgnored(): number { return 3; }\n",
            )
            .unwrap();
            let wt_scan = winb_run(&wt, &home, &["scan", "."]);
            assert!(
                wt_scan.status.success(),
                "worktree scan: {}",
                String::from_utf8_lossy(&wt_scan.stderr)
            );
            assert_hygienic(&wt, "worktree");

            let _ = std::fs::remove_dir_all(&base);
        }

        // -----------------------------------------------------------------------
        // Win-C DB-build dispatch (phase-03 task-4)
        // -----------------------------------------------------------------------

        #[test]
        #[ignore = "e2e tier: real I/O (repo files/scratch repo/spawned apg/db.lbug); run via cargo test-e2e"]
        fn splice_dispatch_seeds_applies_and_publishes() {
            let base =
                std::env::temp_dir().join(format!("apg-splice-dispatch-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&base);
            let apg_root = base.join("apg");
            let trans = apg_root.join(specs::TRANS);
            std::fs::create_dir_all(&trans).unwrap();
            let abs = base.join("a.go").to_string_lossy().into_owned();

            // The previous scan's DB (full load) — the seed source.
            let prev = win_c_fixture(&abs, "old");
            win_c_build_db(&splice::db_path(&apg_root), &prev);

            // The delta graph: a new function in the SAME (target) file and a fresh
            // Scan head; everything else is reused.
            let mut next = prev.clone();
            {
                use crate::graph::{Location, Node, NodeKind};
                next.nodes.insert(
                    "mod.A.g".to_string(),
                    Node {
                        kind: NodeKind::Function,
                        location: Some(Location {
                            path: PathBuf::from(&abs),
                            start: 0,
                            end: 1,
                            start_line: 1,
                            end_line: 1,
                        }),
                        ..Node::default()
                    },
                );
            }
            next.nodes.get_mut(schema::SCAN_HEAD).unwrap().git_sha = Some("new".to_string());
            next.contains
                .insert(("mod.A".to_string(), "mod.A.g".to_string()));

            let input = win_c_input(&base, &base, &["a.go"]);
            let report = with_cwd(&trans, || {
                let mut log = Log::new();
                try_splice_build(&next, &input, &apg_root, &mut log)
            })
            .expect("the incremental dispatch must splice, not fall back");
            assert!(report.scan_refreshed, "the Scan row must be refreshed");
            assert!(
                report.nodes_upserted >= 1,
                "at least the new function is upserted: {report:?}"
            );

            // The published DB answers with the new unit, keeps the unaffected one,
            // and carries the refreshed Scan head.
            let db = Database::new(splice::db_path(&apg_root), SystemConfig::default()).unwrap();
            let conn = Connection::new(&db).unwrap();
            let funcs = emit_json_rows(
                conn.query("MATCH (f:Function) RETURN f.fqn AS fqn")
                    .unwrap(),
            );
            let head = emit_json_rows(
                conn.query("MATCH (s:Scan) RETURN s.git_sha AS sha")
                    .unwrap(),
            );
            drop(conn);
            drop(db);
            assert!(
                funcs.contains("mod.A.g"),
                "the new function must be published: {funcs}"
            );
            assert!(
                funcs.contains("mod.A.f"),
                "an unaffected unit must survive: {funcs}"
            );
            assert!(
                head.contains("new"),
                "the Scan head must be refreshed: {head}"
            );

            // The export is published in the same atomic swap.
            let export = std::fs::read_to_string(splice::export_path(&apg_root)).unwrap();
            assert!(
                export.contains("mod.A.g"),
                "graph.jsonl must carry the new unit"
            );

            let _ = std::fs::remove_dir_all(&base);
        }

        #[test]
        #[ignore = "e2e tier: real I/O (repo files/scratch repo/spawned apg/db.lbug); run via cargo test-e2e"]
        fn splice_dispatch_falls_back_when_ineligible() {
            let base =
                std::env::temp_dir().join(format!("apg-splice-fallback-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&base);
            let apg_root = base.join("apg");
            let trans = apg_root.join(specs::TRANS);
            std::fs::create_dir_all(&trans).unwrap();
            let abs = base.join("a.go").to_string_lossy().into_owned();
            let graph = win_c_fixture(&abs, "new");
            let input = win_c_input(&base, &base, &["a.go"]);
            let db = splice::db_path(&apg_root);

            // No previous DB: the dispatch declines and the full load runs.
            assert!(
                with_cwd(&trans, || {
                    let mut log = Log::new();
                    try_splice_build(&graph, &input, &apg_root, &mut log)
                })
                .is_none(),
                "a missing previous db.lbug must fall back"
            );

            // A previous DB with an unclean WAL sidecar: the whole-file copy could
            // lose unflushed rows, so the dispatch declines.
            win_c_build_db(&db, &graph);
            let wal = format!("{}.wal", db.display());
            std::fs::write(&wal, b"unflushed").unwrap();
            assert!(
                with_cwd(&trans, || {
                    let mut log = Log::new();
                    try_splice_build(&graph, &input, &apg_root, &mut log)
                })
                .is_none(),
                "a WAL sidecar on the previous db must fall back"
            );
            std::fs::remove_file(&wal).unwrap();

            // A full-scan fallback (no phase-2 delta) never splices.
            let mut full = win_c_input(&base, &base, &[]);
            full.reuse = None;
            assert!(
                with_cwd(&trans, || {
                    let mut log = Log::new();
                    try_splice_build(&graph, &full, &apg_root, &mut log)
                })
                .is_none(),
                "the full-scan path must never splice"
            );

            let _ = std::fs::remove_dir_all(&base);
        }

        /// feedback-101: the delta/manifest are shared across worktrees while the
        /// seed is the LOCAL `db.lbug`. When another worktree scans in between, the
        /// shared scan record advances past this worktree's DB; the dispatch must
        /// refuse the seed (falling back to the full load) rather than publish a DB
        /// that is not a full rebuild — and must leave the previous DB untouched.
        #[test]
        #[ignore = "e2e tier: real I/O (repo files/scratch repo/spawned apg/db.lbug); run via cargo test-e2e"]
        fn splice_dispatch_falls_back_when_the_local_seed_is_stale() {
            let base =
                std::env::temp_dir().join(format!("apg-splice-stale-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&base);
            let apg_root = base.join("apg");
            let trans = apg_root.join(specs::TRANS);
            std::fs::create_dir_all(&trans).unwrap();
            let abs = base.join("a.go").to_string_lossy().into_owned();
            let db = splice::db_path(&apg_root);

            // This worktree's local DB was built at content "key-old".
            win_c_build_db(&db, &win_c_fixture(&abs, "old"));
            let before = std::fs::read(&db).unwrap();

            // The shared scan record the delta was derived from names a DIFFERENT
            // tree ("key-other") — another worktree scanned in between.
            let mut input = win_c_input(&base, &base, &["a.go"]);
            input.recorded_content_key = Some("key-other".to_string());
            let next = win_c_fixture(&abs, "new");
            assert!(
                with_cwd(&trans, || {
                    let mut log = Log::new();
                    try_splice_build(&next, &input, &apg_root, &mut log)
                })
                .is_none(),
                "a seed built from a different tree than the shared record must fall back"
            );
            assert_eq!(
                std::fs::read(&db).unwrap(),
                before,
                "the previous DB must be byte-identical when the splice is refused"
            );

            // The common single-worktree case — the shared record names the local
            // DB's own tree — still splices.
            let mut current = win_c_input(&base, &base, &["a.go"]);
            current.recorded_content_key = Some("key-old".to_string());
            let report = with_cwd(&trans, || {
                let mut log = Log::new();
                try_splice_build(&next, &current, &apg_root, &mut log)
            })
            .expect("a current local seed must still splice");
            assert!(
                report.scan_refreshed,
                "the splice must refresh the Scan row"
            );

            let _ = std::fs::remove_dir_all(&base);
        }

        /// Phase-04 task-11 (e2e by body: a REAL scratch-repo `apg scan` — a
        /// process spawn plus `db.lbug` I/O): the emitted per-phase timing report
        /// is first-class and truthful on the real paths. A cold full scan emits
        /// BOTH lines (human `[timing]` + machine `[timing-json]`) carrying all
        /// four phases with NO skip marker; a no-op re-scan of the unchanged repo
        /// takes the freshness fast-path and its report marks the frontend phase
        /// `frontend-skipped`; a content edit forces a normal scan whose report
        /// clears the marker. The in-process model/round-trip assertions stay in
        /// `apg.timing::tests::unit` (task-6); this test supplies the real-scan
        /// coverage the AC requires. Candidate binary only, against a scratch
        /// `/tmp` git repo (`global.constraint.no-real-project-test`).
        #[test]
        #[ignore = "e2e tier: real I/O (scratch repo/spawned apg/db.lbug); run via cargo test-e2e"]
        fn acceptance_scan_emits_per_phase_timing_report_with_fast_path_skip_marker() {
            let base = std::env::temp_dir().join(format!("apg-timing-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&base);
            let repo_dir = base.join("repo");
            let home = base.join("home");
            std::fs::create_dir_all(&home).unwrap();
            // Keep `apg init` hermetic/fast: pre-create the opencode plugin dir so
            // it never shells out to npm.
            std::fs::create_dir_all(home.join(".opencode/node_modules/@opencode-ai/plugin"))
                .unwrap();
            let home_s = home.to_str().unwrap().to_string();

            scratch_repo_init(&repo_dir);
            std::fs::write(repo_dir.join("go.mod"), "module scratch\n\ngo 1.21\n").unwrap();
            std::fs::write(repo_dir.join("main.go"), "package main\n\nfunc main() {}\n").unwrap();
            scratch_commit_all(&repo_dir, "init source");

            let run_in = |dir: &Path, args: &[&str]| -> std::process::Output {
                let out = testutil::ApgCommand::new(args)
                    .cwd(dir)
                    .env("HOME", &home_s)
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
            let stderr_of =
                |out: &std::process::Output| String::from_utf8_lossy(&out.stderr).into_owned();
            let machine_report = |stderr: &str| -> crate::timing::TimingReport {
                let line = stderr
                    .lines()
                    .find(|l| l.starts_with(crate::timing::MACHINE_PREFIX))
                    .unwrap_or_else(|| panic!("no machine-readable timing line in: {stderr}"));
                crate::timing::TimingReport::from_machine_line(line)
                    .unwrap_or_else(|| panic!("the timing line must parse: {line}"))
            };
            let assert_four_phases = |stderr: &str| {
                assert!(stderr.contains("[timing]"), "human line emitted: {stderr}");
                for key in ["startup=", "frontend=", "ingest-assembly=", "db-load="] {
                    assert!(
                        stderr.contains(key),
                        "the timing report must carry {key}: {stderr}"
                    );
                }
            };

            run_in(&repo_dir, &["init", "."]);
            scratch_commit_all(&repo_dir, "apg init");

            // ---- (1) cold full scan: all four phases, no skip marker ----
            let cold = stderr_of(&run_in(&repo_dir, &["scan", "."]));
            assert_four_phases(&cold);
            assert!(
                !cold.contains("frontend-skipped"),
                "a full scan must not mark the frontend skipped: {cold}"
            );
            assert!(
                !machine_report(&cold).frontend_skipped(),
                "a full scan's machine line must clear the skip marker"
            );

            // ---- (2) no-op re-scan: fast-path, all four phases + skip marker ----
            let noop = stderr_of(&run_in(&repo_dir, &["scan", "."]));
            assert!(noop.contains("fast-path"), "fast-path verdict: {noop}");
            assert_four_phases(&noop);
            assert!(
                noop.contains("frontend-skipped"),
                "the fast-path must mark the frontend phase skipped: {noop}"
            );
            assert!(
                machine_report(&noop).frontend_skipped(),
                "the fast-path machine line must carry frontend_skipped"
            );

            // ---- (3) a content edit forces a normal scan: the marker clears ----
            std::fs::write(
                repo_dir.join("main.go"),
                "package main\n\nfunc main() { helper() }\n\nfunc helper() {}\n",
            )
            .unwrap();
            let edited = stderr_of(&run_in(&repo_dir, &["scan", "."]));
            assert_four_phases(&edited);
            assert!(
                !edited.contains("frontend-skipped"),
                "a normal scan clears the skip marker: {edited}"
            );
            assert!(
                !machine_report(&edited).frontend_skipped(),
                "the normal scan's machine line must clear the skip marker"
            );

            let _ = std::fs::remove_dir_all(&base);
        }

        /// Phase-04 task-23 (e2e): a Java TARGETED scan is edge-exact against a
        /// forced from-scratch FULL scan of the same tree. The enumerated oracle
        /// is the per-rel-type counts (`Calls`/`Uses`/`UnresolvedCall`/
        /// `UnresolvedUse`) AND the `UnresolvedTarget` set by FQN-with-categories
        /// — the exact divergence class note-87 measured on jgrapht (graph.jsonl
        /// missing 1 914 / extra 2 145, all resolved -> unresolved), scaled down.
        /// It additionally rejects the javac error-symbol leak: no
        /// `UnresolvedTarget` may carry a project-class simple name or an error
        /// symbol. Candidate binary only, scratch /tmp repo, java-only frontend.
        ///
        /// FIXTURE ROOT (REQUIRED — reproduces the measured root cause): the
        /// sources sit under a MAVEN-LIKE NESTED SOURCE ROOT
        /// (`proj/src/main/java/pkg/...` under the scan root), so the scan root
        /// is NOT a valid package root and the pre-fix `-sourcepath <scan root>`
        /// is silently INEFFECTIVE — the jgrapht Maven multi-module shape. A
        /// fixture with `pkg/a/A.java` directly under the scan root does NOT
        /// reproduce the divergence and this test deliberately does not use it.
        ///
        /// EXTENDED (feedback-134): the fixture also carries a TARGET-package
        /// class (`pkg.b.Target`, referenced from the re-emitted `pkg.b.B`) and an
        /// un-attributable source (`pkg.a.Broken`), so the class dir is
        /// incomplete and the complete project-class index — not the incomplete
        /// class dir — is what must resolve the re-emitted file's references. It
        /// asserts the POSITIVE observable as well: the re-emitted file's calls
        /// and uses into the TARGET-package class appear as their real resolved
        /// FQNs in BOTH scans. Pre-fix (no effective `-sourcepath` context) the
        /// re-emitted package's attribution degrades and this test FAILS — the
        /// resolved edges vanish and a bare project-class simple name leaks — the
        /// same pre-fix FAIL / post-fix PASS the frontend-level fixture
        /// demonstrates (`CallGraphBuilderTest.writeIncompleteContextFixture` /
        /// `testIncompleteClassDirStillResolvesTargetPackage`).
        #[test]
        #[ignore = "e2e tier: real I/O (repo files/scratch repo/spawned apg/db.lbug); run via cargo test-e2e"]
        fn java_targeted_scan_is_edge_exact_against_a_full_scan() {
            let (base, repo_dir, home, frontend_dir) =
                java_scratch("targeted-exact", &java_edge_exactness_fixture());
            let init = java_run(&repo_dir, &home, &frontend_dir, &["init", "."]);
            assert!(
                init.status.success(),
                "apg init: {}",
                String::from_utf8_lossy(&init.stderr)
            );
            scratch_commit_all(&repo_dir, "apg init");

            // Cold full scan: records the manifest + the shared fact store.
            let cold = java_run(&repo_dir, &home, &frontend_dir, &["scan", "."]);
            assert!(
                cold.status.success(),
                "cold scan: {}",
                String::from_utf8_lossy(&cold.stderr)
            );

            // A body-only edit to `pkg.b.B` (same declarations, same exported
            // signature) in the NESTED source root: `pkg.c` references B and
            // `pkg.b` calls into `pkg.a` AND into the TARGET-package class
            // `pkg.b.Target`, so the targeted scan must resolve across the
            // package edges against the complete project-class index even though
            // `pkg.a.Broken` has left the class dir incomplete.
            std::fs::write(
                repo_dir.join("proj/src/main/java/pkg/b/B.java"),
                "package pkg.b;\n\nimport pkg.a.A;\nimport pkg.a.Util;\n\npublic class B {\n    private final A a = new A();\n    private final Target t = new Target();\n\n    public int bar() { return Util.twice(a.foo()) + t.t() + 1; }\n\n    public Target make() { return new Target(); }\n}\n",
            )
            .unwrap();

            let inc = java_run(&repo_dir, &home, &frontend_dir, &["scan", "."]);
            assert!(
                inc.status.success(),
                "targeted scan: {}",
                String::from_utf8_lossy(&inc.stderr)
            );
            let inc_err = String::from_utf8_lossy(&inc.stderr);
            assert!(
                inc_err.contains("[scan] incremental:"),
                "the targeted scan must be incremental, not a full scan: {inc_err}"
            );
            let inc_counts = java_rel_counts(&repo_dir);
            let inc_unresolved = java_unresolved(&repo_dir);
            let inc_calls = java_edges(&repo_dir, "calls");
            let inc_uses = java_edges(&repo_dir, "uses");
            let class_names = java_project_class_simple_names(&repo_dir);
            // Non-vacuity of the nested-root fixture: `pkg.a.Broken` makes the
            // Java compile batch fail, so javac emits NO bytecode and the
            // targeted scan's class dir holds ZERO `.class` files. The exact
            // resolution asserted below therefore came from the corrected
            // `-sourcepath` (the actual source roots), not from the classpath —
            // if this is non-zero the fixture could pass without the fix.
            let class_files = java_class_file_count(&java_store(&repo_dir));
            assert!(
                class_files == 0,
                "the nested-root fixture must leave the class dir EMPTY (the \
                 dropped `pkg.a.Broken` makes the compile batch fail), so the \
                 targeted scan resolves from `-sourcepath`; found {class_files} \
                 `.class` file(s) — the fixture would pass vacuously via the \
                 bytecode classpath"
            );

            // Forced from-scratch FULL scan of the SAME tree: db.lbug +
            // graph.jsonl + the shared fact store cleared — never the
            // incremental run compared against itself.
            std::fs::remove_file(repo_dir.join("apg/.trans/db.lbug")).unwrap();
            std::fs::remove_file(repo_dir.join("apg/.trans/graph.jsonl")).unwrap();
            let _ = std::fs::remove_dir_all(java_store(&repo_dir));
            let full = java_run(&repo_dir, &home, &frontend_dir, &["scan", "."]);
            assert!(
                full.status.success(),
                "full-scan oracle: {}",
                String::from_utf8_lossy(&full.stderr)
            );
            let full_counts = java_rel_counts(&repo_dir);
            let full_unresolved = java_unresolved(&repo_dir);
            let full_calls = java_edges(&repo_dir, "calls");
            let full_uses = java_edges(&repo_dir, "uses");

            assert_eq!(
                inc_counts, full_counts,
                "the per-rel-type counts must be exactly equal (targeted vs forced full)"
            );
            assert_eq!(
                inc_unresolved, full_unresolved,
                "the UnresolvedTarget set by FQN-with-categories must be exactly equal"
            );

            // The POSITIVE observable of the extended fixture: the re-emitted
            // file's references into the TARGET-package class (`pkg.b.Target`)
            // and into the unchanged package (`pkg.a.A`) appear as their real
            // resolved FQNs in the targeted scan AND in the full-scan oracle. A
            // degraded attribution emits a bare project-class simple name (or an
            // error symbol) instead, so this is the assertion the pre-fix
            // implementation fails.
            for (tag, calls, uses) in [
                ("targeted", &inc_calls, &inc_uses),
                ("full", &full_calls, &full_uses),
            ] {
                for (from, to) in [
                    ("java.pkg.b.B.bar", "java.pkg.b.Target.t"),
                    ("java.pkg.b.B.make", "java.pkg.b.Target.<init>"),
                    ("java.pkg.b.B.bar", "java.pkg.a.A.foo"),
                ] {
                    assert!(
                        calls.contains(&(from.to_string(), to.to_string())),
                        "{tag}: the resolved call {from} -> {to} must be present \
                         (a degraded context leaks a bare project-class simple \
                         name instead)"
                    );
                }
                assert!(
                    uses.contains(&(
                        "java.pkg.b.B.make".to_string(),
                        "java.pkg.b.Target".to_string()
                    )),
                    "{tag}: the resolved use pkg.b.B.make -> pkg.b.Target must be present"
                );
            }

            // No bare project-class simple name and no error symbol in EITHER
            // scan's unresolved set (the javac error-symbol leak, note-87).
            for (tag, set) in [("targeted", &inc_unresolved), ("full", &full_unresolved)] {
                for fqn in set.keys() {
                    assert!(
                        !class_names.contains(fqn),
                        "{tag}: UnresolvedTarget {fqn} is a bare project-class simple name"
                    );
                    assert!(
                        !fqn.contains("<error>") && !fqn.to_lowercase().contains("error:"),
                        "{tag}: UnresolvedTarget {fqn} carries a javac error symbol"
                    );
                }
            }

            let _ = std::fs::remove_dir_all(&base);
        }

        /// Phase-04 task-25 (e2e): the first targeted scan after a cold full scan
        /// does not rebuild the Java class cache. With the candidate binary on a
        /// scratch /tmp Java repo and an isolated java-only frontend dir:
        /// (a) the first incremental frontend log reports ZERO
        /// `compiling … unchanged-package file(s)` — the cold full scan seeded
        /// the class cache (phase-04 tasks 33/34 carry the pinned
        /// `--cache-dir`/`--cache-key` hand-off through the full-scan path), and
        /// the pre-fix rebuild of every unchanged file fails this; (b) the
        /// incremental graph still equals a full scan of the same changed tree
        /// under the enumerated per-rel-type + UnresolvedTarget oracle; (c) the
        /// control — RE-POINTED (feedback-131) at a SEPARATE invocation that
        /// genuinely has no cache hand-off, NEVER the (a) cold scan: once tasks
        /// 33/34 land, that cold scan always carries the cache flags on the
        /// full-scan branch, so requiring it to write no artifact is
        /// unsatisfiable. Invocation: a full scan in a NON-GIT scratch layout,
        /// where `FactStore::resolve` fails and `incremental::prepare` returns an
        /// empty `store_root` with `FullScanReason::NotAGitRepo`, so
        /// `handoff.cache_dir`/`handoff.cache_key` stay `None`. It asserts
        /// non-vacuously that (i) that run's frontend log carries NO
        /// `seeding java class cache for …` line, (ii) NO `surface.tsv`/`classes/`
        /// artifact exists ANYWHERE under that scratch tree — not merely under
        /// the (a) store, so a fabricated/defaulted cache path is caught — and
        /// (iii) the full scan itself SUCCEEDED (a real invocation, not a no-op).
        ///
        /// (a) and (b) are asserted exactly as filed and are never weakened.
        #[test]
        #[ignore = "e2e tier: real I/O (repo files/scratch repo/spawned apg/db.lbug); run via cargo test-e2e"]
        fn java_first_targeted_scan_after_a_seeded_full_scan_compiles_no_unchanged_package() {
            let (base, repo_dir, home, frontend_dir) =
                java_scratch("class-cache-seed", &java_seeding_fixture());
            let init = java_run(&repo_dir, &home, &frontend_dir, &["init", "."]);
            assert!(
                init.status.success(),
                "apg init: {}",
                String::from_utf8_lossy(&init.stderr)
            );
            scratch_commit_all(&repo_dir, "apg init");

            // Cold FULL scan. With phase-04 tasks 33/34 the full-scan branch now
            // carries the pinned `--cache-dir`/`--cache-key` hand-off (and still
            // NO `--targets`), so this scan seeds the Java class surface that the
            // first targeted re-scan consumes.
            let cold = java_run(&repo_dir, &home, &frontend_dir, &["scan", "."]);
            assert!(
                cold.status.success(),
                "cold full scan: {}",
                String::from_utf8_lossy(&cold.stderr)
            );
            let store = java_store(&repo_dir);
            assert!(
                store.is_dir(),
                "the cold scan must record the shared fact store"
            );
            // Task-33 AC (b): the cold full scan — now carrying the pinned cache
            // hand-off — must actually seed the Java class surface the first
            // targeted re-scan consumes (the mechanism behind (a)).
            assert!(
                java_class_cache_seeded(&store),
                "the cold full scan must seed the java class cache under {}",
                store.display()
            );

            // Localized body-only edit of the target package.
            std::fs::write(
                repo_dir.join("pkg/target/T.java"),
                "package pkg.target;\n\nimport pkg.dep.Dep;\n\npublic class T {\n    public int go() { return new Dep().v() + 1; }\n}\n",
            )
            .unwrap();

            // First targeted scan after the cold full scan.
            let inc = java_run(&repo_dir, &home, &frontend_dir, &["scan", "."]);
            assert!(
                inc.status.success(),
                "targeted scan: {}",
                String::from_utf8_lossy(&inc.stderr)
            );
            let inc_err = String::from_utf8_lossy(&inc.stderr);
            assert!(
                inc_err.contains("[scan] incremental:"),
                "the first re-scan must be incremental: {inc_err}"
            );

            // (a) the seeded-cache observable: ZERO unchanged-package compiles.
            let log_path = repo_dir.join("apg/.trans/apg-frontend.log");
            let log = std::fs::read_to_string(&log_path)
                .unwrap_or_else(|e| panic!("read {}: {e}", log_path.display()));
            assert!(
                log.contains("running java frontend"),
                "the java frontend must have run: {log}"
            );
            let compiled = java_unchanged_package_compiles(&log).unwrap_or(0);

            let inc_counts = java_rel_counts(&repo_dir);
            let inc_unresolved = java_unresolved(&repo_dir);

            // (b) forced from-scratch FULL scan oracle of the same changed tree.
            std::fs::remove_file(repo_dir.join("apg/.trans/db.lbug")).unwrap();
            std::fs::remove_file(repo_dir.join("apg/.trans/graph.jsonl")).unwrap();
            let _ = std::fs::remove_dir_all(&store);
            let full = java_run(&repo_dir, &home, &frontend_dir, &["scan", "."]);
            assert!(
                full.status.success(),
                "full-scan oracle: {}",
                String::from_utf8_lossy(&full.stderr)
            );
            let full_counts = java_rel_counts(&repo_dir);
            let full_unresolved = java_unresolved(&repo_dir);

            // (c) control — the genuine no-hand-off case: a full scan in a
            // SEPARATE, NON-GIT scratch layout. `FactStore::resolve` fails there,
            // so `incremental::prepare` returns `FullScanReason::NotAGitRepo`
            // with an empty `store_root` and the hand-off carries neither cache
            // flag. Assert (i) no seeding line, (ii) no class-cache artifact
            // ANYWHERE under this scratch tree, (iii) the full scan succeeded.
            let (nongit_base, nongit_repo, nongit_home, nongit_frontend_dir) =
                java_scratch("class-cache-nogit", &java_seeding_fixture());
            let nongit_init = java_run(
                &nongit_repo,
                &nongit_home,
                &nongit_frontend_dir,
                &["init", "."],
            );
            assert!(
                nongit_init.status.success(),
                "non-git control apg init: {}",
                String::from_utf8_lossy(&nongit_init.stderr)
            );
            scratch_commit_all(&nongit_repo, "apg init");
            // Strip `.git` AFTER init: the layout stays versioned, but
            // `FactStore::resolve` can no longer find a store (NotAGitRepo).
            std::fs::remove_dir_all(nongit_repo.join(".git")).unwrap();
            let nongit = java_run(
                &nongit_repo,
                &nongit_home,
                &nongit_frontend_dir,
                &["scan", "."],
            );
            let mut c_failures: Vec<String> = Vec::new();
            if !nongit.status.success() {
                c_failures.push(format!(
                    "(c) the no-hand-off full scan failed: {}",
                    String::from_utf8_lossy(&nongit.stderr)
                ));
            }
            let nongit_log_path = nongit_repo.join("apg/.trans/apg-frontend.log");
            let nongit_log = std::fs::read_to_string(&nongit_log_path)
                .unwrap_or_else(|e| panic!("read {}: {e}", nongit_log_path.display()));
            // (iii) a real invocation, not a no-op: the java frontend ran and the
            // scan produced a graph.
            if !nongit_log.contains("running java frontend") {
                c_failures.push("(c) the no-hand-off scan never ran the java frontend".to_string());
            }
            if !nongit_repo.join("apg/.trans/graph.jsonl").is_file() {
                c_failures.push("(c) the no-hand-off scan wrote no graph.jsonl".to_string());
            }
            // (i) no seeding line (there was no --cache-dir/--cache-key flag).
            if nongit_log.contains("seeding java class cache for") {
                c_failures
                    .push("(c) the no-hand-off full scan seeded the java class cache".to_string());
            }
            // (ii) no class-cache artifact ANYWHERE under the control's scratch
            // tree — a fabricated/defaulted cache path is caught here.
            let mut artifacts: Vec<String> = Vec::new();
            collect_class_cache_artifacts(&nongit_base, &mut artifacts);
            if !artifacts.is_empty() {
                c_failures.push(format!(
                    "(c) the no-hand-off full scan wrote class-cache artifacts: {artifacts:?}"
                ));
            }
            let _ = std::fs::remove_dir_all(&nongit_base);

            // Report ALL THREE ACs from one run: the test still fails if any is
            // violated, but a failure names every measured number.
            let mut failures: Vec<String> = Vec::new();
            failures.extend(c_failures);
            if compiled != 0 {
                failures.push(format!(
                    "(a) the first targeted scan compiled {compiled} unchanged-package file(s) \
                     (expected ZERO — the cold full scan seeded the class cache)"
                ));
            }
            if inc_counts != full_counts {
                failures.push(format!(
                    "(b) per-rel-type counts differ: incremental {inc_counts:?} vs full {full_counts:?}"
                ));
            }
            if inc_unresolved != full_unresolved {
                failures.push(format!(
                    "(b) UnresolvedTarget sets differ: incremental {inc_unresolved:?} vs full {full_unresolved:?}"
                ));
            }
            assert!(
                failures.is_empty(),
                "task-25 AC(s) violated:\n{}\n\nLog tail:\n{}",
                failures.join("\n"),
                log.lines().rev().take(12).collect::<Vec<_>>().join("\n")
            );

            let _ = std::fs::remove_dir_all(&base);
        }

        /// Phase-04 task-3 (e2e): the candidate `apg` scan of a scratch /tmp
        /// DEFAULT-PACKAGE Java repo — no `package` declaration anywhere — must
        /// materialise a `java` Language root that Contains the non-empty
        /// `(default)` module, hang the package-less File under that module,
        /// render the top-level class with no leading dot
        /// (`java.(default).Widget`), and expose the whole
        /// Module→File→Struct subtree to the module-based tools (the
        /// `apg_module_structs` two-hop query). Candidate binary only, scratch
        /// /tmp repo, isolated java-only frontend dir
        /// (`global.constraint.no-real-project-test`).
        #[test]
        #[ignore = "e2e tier: real I/O (repo files/scratch repo/spawned apg/db.lbug); run via cargo test-e2e"]
        fn java_default_package_identity_end_to_end() {
            let (base, repo_dir, home, frontend_dir) = java_scratch(
                "default-package",
                &[
                    (
                        "Widget.java",
                        "public class Widget {\n    public int size() { return 1; }\n}\n",
                    ),
                    (
                        "Gadget.java",
                        "class Gadget {\n    static class Inner { int v() { return 2; } }\n}\n",
                    ),
                ],
            );
            let init = java_run(&repo_dir, &home, &frontend_dir, &["init", "."]);
            assert!(
                init.status.success(),
                "apg init: {}",
                String::from_utf8_lossy(&init.stderr)
            );
            scratch_commit_all(&repo_dir, "apg init");

            let scan = java_run(&repo_dir, &home, &frontend_dir, &["scan", "."]);
            assert!(
                scan.status.success(),
                "scan: {}",
                String::from_utf8_lossy(&scan.stderr)
            );

            // The graph view: the `java` Language root, the default-package
            // module, the package-less File and the top-level structs.
            let records = export_records(&repo_dir);
            let nodes: BTreeSet<(String, String)> = records
                .iter()
                .filter_map(|r| {
                    let ty = r.get("type").and_then(|t| t.as_str())?;
                    let fqn = r.get("fqn").and_then(|f| f.as_str())?;
                    matches!(ty, "language" | "module" | "file" | "struct")
                        .then(|| (ty.to_string(), fqn.to_string()))
                })
                .collect();
            let contains: BTreeSet<(String, String)> = records
                .iter()
                .filter(|r| r.get("type").and_then(|t| t.as_str()) == Some("contains"))
                .map(|r| {
                    (
                        r.get("from")
                            .and_then(|f| f.as_str())
                            .unwrap_or("")
                            .to_string(),
                        r.get("to")
                            .and_then(|t| t.as_str())
                            .unwrap_or("")
                            .to_string(),
                    )
                })
                .collect();

            assert!(
                nodes.contains(&("language".to_string(), "java".to_string())),
                "the scan must materialise a `java` Language root; nodes: {nodes:?}"
            );
            assert!(
                nodes.contains(&("module".to_string(), "java.(default)".to_string())),
                "the default package must emit a non-empty module record; nodes: {nodes:?}"
            );
            assert!(
                contains.contains(&("java".to_string(), "java.(default)".to_string())),
                "the `java` Language root must Contain the default-package module; \
                 contains: {contains:?}"
            );

            // The package-less File hangs under the non-empty module: the
            // File's parent is the Module→File Contains edge (the export carries
            // the repo-relative File fqn, never the absolute checkout path).
            let widget_file = nodes
                .iter()
                .find(|(ty, fqn)| ty == "file" && fqn.ends_with("Widget.java"))
                .map(|(_, fqn)| fqn.clone())
                .unwrap_or_else(|| panic!("no File record for Widget.java; nodes: {nodes:?}"));
            assert!(
                !widget_file.starts_with('/'),
                "the File fqn is the repo-relative identity: {widget_file}"
            );
            assert!(
                contains.contains(&("java.(default)".to_string(), widget_file.clone())),
                "the package-less File's parent must be the default-package module; \
                 contains: {contains:?}"
            );

            // The top-level class renders with the module parent and NO leading
            // dot: `java.(default).Widget` (a bare `.Widget` would be the
            // package-less bug this phase fixes).
            assert!(
                nodes.contains(&("struct".to_string(), "java.(default).Widget".to_string())),
                "the top-level class fqn must be `java.(default).Widget`; nodes: {nodes:?}"
            );
            assert!(
                !nodes
                    .iter()
                    .any(|(ty, fqn)| ty.as_str() == "struct" && fqn.starts_with('.')),
                "no struct fqn may carry a leading dot; nodes: {nodes:?}"
            );
            assert!(
                contains.contains(&(widget_file.clone(), "java.(default).Widget".to_string())),
                "the File must Contain its top-level class; contains: {contains:?}"
            );

            // Module-based tools: the `apg_module_structs` two-hop query — run
            // through the candidate `apg query` against the scratch DB —
            // enumerates the Module→File→Struct subtree.
            let q = java_run(
                &repo_dir,
                &home,
                &frontend_dir,
                &[
                    "query",
                    "MATCH (m:Module {fqn: 'java.(default)'})-[:Contains]->(:File)-[:Contains]->(s:Struct) RETURN s.fqn ORDER BY s.fqn",
                ],
            );
            assert!(
                q.status.success(),
                "module_structs query: {}",
                String::from_utf8_lossy(&q.stderr)
            );
            let qout = String::from_utf8_lossy(&q.stdout).into_owned();
            assert!(
                qout.contains("java.(default).Widget"),
                "apg_module_structs must enumerate the default-package class: {qout}"
            );
            assert!(
                qout.contains("java.(default).Gadget"),
                "apg_module_structs must enumerate every default-package class: {qout}"
            );

            let _ = std::fs::remove_dir_all(&base);
        }

        // -------------------------------------------------------------------
        // Phase-05 Rust all-manifest discovery / isolation acceptance
        // (tasks 5, 6, 9). Scratch /tmp `Repo::new` fixtures + CANDIDATE binary
        // only — never a real checkout (global.constraint.no-real-project-test).
        // A real rust-frontend build + scan is real I/O, so every one is
        // `#[ignore]`-and-invoke (global.constraint.test-tier-boundaries).
        // -------------------------------------------------------------------

        /// Phase-05 task-5 (e2e): fixture discovery acceptance — (a) a nested
        /// non-workspace crate yields BOTH its Module node and its symbols;
        /// (b) a `[workspace]` yields each project exactly once; (c) a nested
        /// Cargo project under `.worktrees/` is NOT discovered; (d) a generated
        /// `.rs` under `target/` (incl. the `src/rustlib/target/**/out/*.rs`
        /// shape) produces NO code node. Scratch /tmp fixtures, candidate
        /// binary, opt-in only.
        #[test]
        #[ignore = "e2e tier: real I/O (scratch repo/spawned apg/rust frontend build); run via cargo test-e2e"]
        fn rust_all_manifest_discovery_nested_workspace_and_exclusions() {
            let home =
                std::env::temp_dir().join(format!("apg-rustdisc-home-{}", std::process::id()));
            std::fs::create_dir_all(&home).unwrap();

            // (a) NESTED NON-WORKSPACE CRATE: root package + nested package with
            // its own manifest/lock, NOT a `[workspace]` member. BOTH the nested
            // crate's Module node AND its declared symbols must appear.
            let nested = crate::testutil::Repo::new("rust-disc-nested");
            nested.write(
                "Cargo.toml",
                "[package]\nname = \"root-app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            );
            nested.write(
                "src/lib.rs",
                "pub struct RootThing;\npub fn root_fn() -> i32 { 1 }\n",
            );
            nested.write(
                "nested/Cargo.toml",
                "[package]\nname = \"nested-app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            );
            nested.write(
                "nested/Cargo.lock",
                "# This file is automatically @generated by Cargo.\nversion = 4\n\n[[package]]\nname = \"nested-app\"\nversion = \"0.1.0\"\n",
            );
            nested.write(
                "nested/src/lib.rs",
                "pub struct NestedThing;\npub fn nested_fn() -> i32 { 2 }\n",
            );
            nested.commit_all("fixture");
            let scan = winb_run(&nested.root, &home, &["scan", ".", "--language", "rust"]);
            assert!(
                scan.status.success(),
                "(a) nested non-workspace scan failed:\n{}",
                String::from_utf8_lossy(&scan.stderr)
            );
            let recs = export_records(&nested.root);
            let modules = export_module_counts(&recs);
            assert_eq!(
                modules.get("rust.nested-app"),
                Some(&1),
                "(a) the nested crate's Module node must appear exactly once: {modules:?}"
            );
            assert_eq!(
                modules.get("rust.root-app"),
                Some(&1),
                "(a) root module: {modules:?}"
            );
            let symbols = export_symbol_fqns(&recs);
            assert!(
                symbols.contains("rust.nested-app.NestedThing")
                    && symbols.contains("rust.nested-app.nested_fn"),
                "(a) the nested crate's declared symbols must appear: {symbols:?}"
            );
            assert!(
                symbols.contains("rust.root-app.RootThing"),
                "(a) the root crate's symbols must appear: {symbols:?}"
            );
            let _ = std::fs::remove_dir_all(&nested.root);

            // (b) WORKSPACE: each member exactly once, discovered-project count
            // equal to the workspace's project count (2).
            let ws = crate::testutil::Repo::new("rust-disc-workspace");
            ws.write(
                "Cargo.toml",
                "[workspace]\nmembers = [\"crate-a\", \"crate-b\"]\nresolver = \"2\"\n",
            );
            ws.write(
                "crate-a/Cargo.toml",
                "[package]\nname = \"crate-a\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            );
            ws.write("crate-a/src/lib.rs", "pub struct AThing;\n");
            ws.write(
                "crate-b/Cargo.toml",
                "[package]\nname = \"crate-b\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            );
            ws.write("crate-b/src/lib.rs", "pub struct BThing;\n");
            ws.commit_all("fixture");
            let scan = winb_run(&ws.root, &home, &["scan", ".", "--language", "rust"]);
            assert!(
                scan.status.success(),
                "(b) workspace scan failed:\n{}",
                String::from_utf8_lossy(&scan.stderr)
            );
            let recs = export_records(&ws.root);
            let modules = export_module_counts(&recs);
            assert_eq!(
                modules.get("rust.crate-a"),
                Some(&1),
                "(b) crate-a must be discovered exactly once: {modules:?}"
            );
            assert_eq!(
                modules.get("rust.crate-b"),
                Some(&1),
                "(b) crate-b must be discovered exactly once: {modules:?}"
            );
            // Count only the RUST crate modules: the bundled structural scanner
            // now legitimately mints `toml.`/`json.`/`misc.` modules for the
            // tracked manifest/config files, so the total module set is no
            // longer code-only. `rust.` is the Rust stream root, so this scopes
            // the discovered-project count to the crates under test.
            let rust_project_total: usize = modules
                .iter()
                .filter(|(fqn, _)| fqn.starts_with("rust."))
                .map(|(_, n)| *n)
                .sum();
            assert_eq!(
                rust_project_total, 2,
                "(b) discovered Rust projects must equal the workspace's 2 projects: {modules:?}"
            );
            let symbols = export_symbol_fqns(&recs);
            assert!(
                symbols.contains("rust.crate-a.AThing") && symbols.contains("rust.crate-b.BThing"),
                "(b) each member's symbols must appear: {symbols:?}"
            );
            let _ = std::fs::remove_dir_all(&ws.root);

            // (c) `.worktrees/` EXCLUSION: a nested Cargo project under it is not
            // discovered (no module node; its FQNs absent).
            let wt = crate::testutil::Repo::new("rust-disc-worktrees");
            wt.write(
                "Cargo.toml",
                "[package]\nname = \"wt-root\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            );
            wt.write("src/lib.rs", "pub struct WtRoot;\n");
            wt.write(
                ".worktrees/hidden/Cargo.toml",
                "[package]\nname = \"hidden-app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            );
            wt.write(".worktrees/hidden/src/lib.rs", "pub struct HiddenThing;\n");
            wt.commit_all("fixture");
            let scan = winb_run(&wt.root, &home, &["scan", ".", "--language", "rust"]);
            assert!(
                scan.status.success(),
                "(c) .worktrees scan failed:\n{}",
                String::from_utf8_lossy(&scan.stderr)
            );
            let recs = export_records(&wt.root);
            let modules = export_module_counts(&recs);
            assert!(
                !modules.contains_key("hidden-app"),
                "(c) a crate under .worktrees/ must not be discovered: {modules:?}"
            );
            let symbols = export_symbol_fqns(&recs);
            assert!(
                !symbols.iter().any(|f| f.contains("HiddenThing")),
                "(c) a .worktrees/ crate's symbols must be absent: {symbols:?}"
            );
            assert!(
                symbols.contains("rust.wt-root.WtRoot"),
                "(c) the real root crate must still be discovered: {symbols:?}"
            );
            let _ = std::fs::remove_dir_all(&wt.root);

            // (d) GENERATED-TREE EXCLUSION. PART 1: a Cargo project under
            // `target/` is never a scan root. PART 2: no File/Module/Struct/
            // Function may carry a path under `target/` — including the
            // `src/rustlib/target/**/out/*.rs` shape, where a nested crate sits
            // beside its own generated tree.
            let gen_repo = crate::testutil::Repo::new("rust-disc-generated");
            gen_repo.write(
                "Cargo.toml",
                "[package]\nname = \"gen-root\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            );
            gen_repo.write("src/lib.rs", "pub struct GenRoot;\n");
            gen_repo.write(
                "target/nested-crate/Cargo.toml",
                "[package]\nname = \"target-crate\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            );
            gen_repo.write(
                "target/nested-crate/src/lib.rs",
                "pub struct TargetCrateThing;\n",
            );
            gen_repo.write(
                "target/gen/out/generated.rs",
                "pub struct GeneratedInTarget;\n",
            );
            gen_repo.write(
                "src/rustlib/Cargo.toml",
                "[package]\nname = \"rustlib-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            );
            gen_repo.write("src/rustlib/src/lib.rs", "pub struct RustlibThing;\n");
            gen_repo.write(
                "src/rustlib/target/debug/build/x/out/generated.rs",
                "pub struct OutDirThing;\n",
            );
            gen_repo.commit_all("fixture");
            let scan = winb_run(&gen_repo.root, &home, &["scan", ".", "--language", "rust"]);
            assert!(
                scan.status.success(),
                "(d) generated-tree scan failed:\n{}",
                String::from_utf8_lossy(&scan.stderr)
            );
            let recs = export_records(&gen_repo.root);
            let symbols = export_symbol_fqns(&recs);
            // Non-vacuous: the crate BESIDE the generated tree was loaded, so
            // the exclusion is exercised against a live scan, not an empty one.
            assert!(
                symbols
                    .iter()
                    .any(|f| f == "rust.rustlib-fixture.RustlibThing"),
                "(d) the crate beside the generated tree must be discovered: {symbols:?}"
            );
            let leaked: Vec<String> = export_code_locations(&recs)
                .into_iter()
                .filter(|p| under_component(&gen_repo.root, p, "target"))
                .collect();
            assert!(
                leaked.is_empty(),
                "(d) no code node may be drawn from a generated target/ path: {leaked:?}"
            );
            for absent in ["GeneratedInTarget", "TargetCrateThing", "OutDirThing"] {
                assert!(
                    !symbols.iter().any(|f| f.contains(absent)),
                    "(d) `{absent}` must never be a code node: {symbols:?}"
                );
            }
            assert!(
                !export_module_counts(&recs).contains_key("target-crate"),
                "(d) a Cargo project under target/ must not be discovered"
            );
            let _ = std::fs::remove_dir_all(&gen_repo.root);

            let _ = std::fs::remove_dir_all(&home);
        }

        /// Phase-05 task-6 (e2e): nested-FQN acceptance on a scratch /tmp fixture
        /// reproducing the src/rustlib condition — ROOT package `apg` + NESTED
        /// non-workspace crate `apg-rustfrontend` (own manifest + lock, two
        /// differing package names). Candidate binary only; no real checkout.
        #[test]
        #[ignore = "e2e tier: real I/O (scratch repo/spawned apg/rust frontend build); run via cargo test-e2e"]
        fn rust_nested_fqn_clean_and_unshadowed() {
            let home =
                std::env::temp_dir().join(format!("apg-rustfqn-home-{}", std::process::id()));
            std::fs::create_dir_all(&home).unwrap();
            let repo = crate::testutil::Repo::new("rust-nested-fqn");
            repo.write(
                "Cargo.toml",
                "[package]\nname = \"apg\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            );
            repo.write(
                "src/lib.rs",
                "pub struct RootApi;\npub fn root_entry() -> i32 { 1 }\n",
            );
            repo.write(
                "src/rustlib/Cargo.toml",
                "[package]\nname = \"apg-rustfrontend\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[[bin]]\nname = \"rustfrontend\"\npath = \"src/main.rs\"\n",
            );
            repo.write(
                "src/rustlib/Cargo.lock",
                "# This file is automatically @generated by Cargo.\nversion = 4\n\n[[package]]\nname = \"apg-rustfrontend\"\nversion = \"0.1.0\"\n",
            );
            repo.write(
                "src/rustlib/src/main.rs",
                "pub struct Scanner;\nfn main() {}\n",
            );
            repo.commit_all("fixture");

            // (1) NO same-kind collision panic: the scan exits 0 with no `claim`
            // panic text.
            let scan = winb_run(&repo.root, &home, &["scan", ".", "--language", "rust"]);
            let stdout = String::from_utf8_lossy(&scan.stdout).into_owned();
            let stderr = String::from_utf8_lossy(&scan.stderr).into_owned();
            assert!(scan.status.success(), "(1) scan must exit 0:\n{stderr}");
            for needle in ["FQN collision", "panicked", "duplicate project node"] {
                assert!(
                    !stdout.contains(needle) && !stderr.contains(needle),
                    "(1) scan output must carry no `{needle}`:\n{stdout}\n{stderr}"
                );
            }

            // (2) ZERO SHADOWED MODULES — positively, by feeding the fixture's
            // emitted scanner records through the in-process ingestor. The CLI
            // logs the shadow warning only when the count is > 0, so the
            // absence of that line alone is vacuous.
            let records = rust_frontend_records(&repo.root);
            assert!(
                !records.is_empty(),
                "(2) the rust frontend must emit scanner records"
            );
            let (_graph, report) = crate::ingest::ingest(
                records,
                &crate::ingest::IngestOptions {
                    blacklist: &[],
                    language: "rust",
                    config: None,
                    base: None,
                },
            );
            assert_eq!(
                report.shadowed_modules, 0,
                "(2) the nested crate's module must not be shadowed"
            );
            assert_eq!(
                report.shadowed_functions, 0,
                "(2) no function may be shadowed"
            );
            assert!(
                !stdout.contains("module(s) shadowed by a type of the same name")
                    && !stderr.contains("module(s) shadowed by a type of the same name"),
                "(2) the real scan must emit no shadow line:\n{stdout}\n{stderr}"
            );

            // (3) ROOTING-AGNOSTIC FQN cleanliness: normalize a leading `<lang>.`
            // root away, then assert the nested crate's identity is
            // `apg-rustfrontend` exactly once, disjoint from the root package's
            // `apg`, with each crate's symbols under its own identity.
            let recs = export_records(&repo.root);
            let modules: std::collections::BTreeMap<String, usize> = export_module_counts(&recs)
                .into_iter()
                .map(|(k, v)| (strip_lang_root(&k), v))
                .collect();
            assert_eq!(
                modules.get("apg-rustfrontend"),
                Some(&1),
                "(3) the nested crate's identity must be present exactly once: {modules:?}"
            );
            assert_eq!(
                modules.get("apg"),
                Some(&1),
                "(3) the root package's identity must be present exactly once: {modules:?}"
            );
            let symbols: BTreeSet<String> = export_symbol_fqns(&recs)
                .into_iter()
                .map(|f| strip_lang_root(&f))
                .collect();
            assert!(
                symbols.contains("apg-rustfrontend.Scanner"),
                "(3) the nested crate's symbol must hang under its own identity: {symbols:?}"
            );
            assert!(
                symbols.contains("apg.RootApi"),
                "(3) the root package's symbol must hang under `apg`: {symbols:?}"
            );

            let _ = std::fs::remove_dir_all(&repo.root);
            let _ = std::fs::remove_dir_all(&home);
        }

        /// Phase-05 task-9 (e2e): Rust build-isolation acceptance, in-process
        /// over the real repo — the same pattern as
        /// `cargo_manifest_and_lockfile_declare_release_version`. Reads the real
        /// `Cargo.toml`/`Cargo.lock`/`build.rs` and the real build artifacts
        /// under `src/rustlib/target/` (real filesystem I/O), so it is
        /// `#[ignore]`-and-invoke. (A) root workspace membership unchanged;
        /// (B) a separate rustlib lockfile distinct from the root's;
        /// (C) build.rs still compiles rustlib into its isolated target dir.
        #[test]
        #[ignore = "e2e tier: real I/O (repo files/build artifacts under src/rustlib/target); run via cargo test-e2e"]
        fn rust_build_isolation_preserved() {
            let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

            // (A) ROOT WORKSPACE MEMBERSHIP UNCHANGED: no `[workspace]` table and
            // no `members` entry naming `src/rustlib`.
            let root_manifest = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
            assert!(
                !root_manifest.contains("[workspace]"),
                "(A) the root Cargo.toml must declare no [workspace] table"
            );
            assert!(
                !root_manifest.contains("src/rustlib"),
                "(A) the root Cargo.toml must not name src/rustlib (build isolation)"
            );
            assert!(
                !root_manifest
                    .lines()
                    .any(|l| l.trim().starts_with("members")),
                "(A) the root Cargo.toml must declare no workspace `members`"
            );

            // (B) SEPARATE LOCKFILE: src/rustlib is a standalone `[package]` with
            // its own lock carrying the pinned rust-analyzer git deps, DISTINCT
            // from the root lock (a merged/root lock would mean membership).
            let rustlib_manifest =
                std::fs::read_to_string(root.join("src/rustlib/Cargo.toml")).unwrap();
            assert!(
                rustlib_manifest.contains("[package]")
                    && rustlib_manifest.contains("name = \"apg-rustfrontend\""),
                "(B) src/rustlib/Cargo.toml must be a standalone [package]"
            );
            assert!(
                rustlib_manifest.contains("edition = \"2021\""),
                "(B) src/rustlib must stay edition 2021"
            );
            let root_lock = std::fs::read_to_string(root.join("Cargo.lock")).unwrap();
            let rustlib_lock_path = root.join("src/rustlib/Cargo.lock");
            assert!(
                rustlib_lock_path.is_file(),
                "(B) src/rustlib/Cargo.lock must exist"
            );
            let rustlib_lock = std::fs::read_to_string(&rustlib_lock_path).unwrap();
            assert!(
                rustlib_lock.contains("name = \"apg-rustfrontend\""),
                "(B) the rustlib lock must pin its own package"
            );
            assert!(
                rustlib_lock.contains("git+https://github.com/rust-lang/rust-analyzer"),
                "(B) the rustlib lock must carry the pinned rust-analyzer git-dependency crates"
            );
            assert_ne!(
                root_lock, rustlib_lock,
                "(B) the rustlib lock must be distinct from the root lock"
            );
            assert!(
                !root_lock.contains("apg-rustfrontend")
                    && !root_lock.contains("git+https://github.com/rust-lang/rust-analyzer"),
                "(B) the root lock must not carry the rustlib package or its git deps"
            );

            // (C) BUILD.RS STILL COMPILES RUSTLIB IN ISOLATION: `cargo build
            // --manifest-path src/rustlib/Cargo.toml ... --release --bin
            // rustfrontend`, targeting the isolated
            // `src/rustlib/target/release/` (ALWAYS `--release`, independent of
            // the outer cargo profile).
            let build_rs = std::fs::read_to_string(root.join("build.rs")).unwrap();
            assert!(
                build_rs.contains("--manifest-path") && build_rs.contains("src/rustlib/Cargo.toml"),
                "(C) build.rs must build via --manifest-path src/rustlib/Cargo.toml"
            );
            assert!(
                build_rs.contains("--bin") && build_rs.contains("rustfrontend"),
                "(C) build.rs must build the rustfrontend bin"
            );
            // build.rs compiles rustlib with `--release` into the isolated
            // `src/rustlib/target/release/` regardless of the outer cargo
            // profile, then stages it into the active profile's frontends dir.
            let isolated = root.join("src/rustlib/target/release/rustfrontend");
            assert!(
                isolated.is_file(),
                "(C) the rustfrontend must be built into the ISOLATED target dir {} — \
                 run `cargo build` (build.rs compiles it there)",
                isolated.display()
            );
            let profile_dir = crate::testutil::apg_bin()
                .parent()
                .expect("apg binary has a parent")
                .to_path_buf();
            let root_artifact = profile_dir.join("rustfrontend");
            assert!(
                !root_artifact.exists(),
                "(C) the rustfrontend must NOT be compiled into the root target dir: {}",
                root_artifact.display()
            );
        }

        /// Phase-06 task-12 (e2e): a JS-only scratch repo exercising ALL FOUR
        /// accepted JS extensions (`.js`/`.jsx`/`.mjs`/`.cjs`) through the real
        /// candidate binary + the built unified frontend. Each extension is
        /// ACCEPTED and yields a File node, the module, its declared symbols, and
        /// a resolve-only edge; `node_modules` is skipped entirely.
        #[test]
        #[ignore = "e2e tier: real I/O (scratch /tmp repo scanned by the candidate binary); run via cargo test-e2e"]
        fn js_only_repo_accepts_all_four_js_extensions() {
            assert!(ts_frontend_artifact().is_file());
            let (base, repo_dir, home) = js_scratch("js-only", &js_only_fixture());
            let scan = winb_run(&repo_dir, &home, &["scan", "."]);
            let stderr = String::from_utf8_lossy(&scan.stderr).to_string();
            assert!(scan.status.success(), "scan failed: {stderr}");

            let records = export_records(&repo_dir);
            let files = export_file_paths(&records);
            // fix-module-identity phase-02: a package-less repo identifies its
            // one module from the repo-relative base (`root`), never the
            // checkout/scan-root directory basename (`repo`). The old synthetic
            // `js.repo` module is gone
            // (`requirements.constraint.no-checkout-named-module`).
            assert!(
                !export_modules(&records).contains("js.repo"),
                "no module may be named after the checkout/scan-root basename: {:?}",
                export_modules(&records)
            );

            for rel in ["calc.js", "widget.jsx", "esm.mjs", "cjs.cjs"] {
                let abs = export_file_ending(&records, &format!("/{rel}"))
                    .unwrap_or_else(|| panic!("{rel} must be accepted as a File node: {files:?}"));
                // Exactly ONE module identity parents each source file: no file
                // is emitted twice (once per recursive package walk).
                let parents: BTreeSet<String> = records
                    .iter()
                    .filter(|r| r.get("type").and_then(|t| t.as_str()) == Some("contains"))
                    .filter(|r| r.get("to").and_then(|t| t.as_str()) == Some(abs.as_str()))
                    .filter_map(|r| r.get("from").and_then(|f| f.as_str()).map(str::to_string))
                    .collect();
                assert_eq!(
                    parents.len(),
                    1,
                    "{rel} must be emitted under exactly one module identity: {parents:?}"
                );
                let syms = export_symbols_at(&records, &abs);
                assert!(!syms.is_empty(), "{rel} must declare symbols: {syms:?}");
                let edges = export_edge_types_from(&records, &syms);
                assert!(
                    !edges.is_empty(),
                    "{rel} must emit a resolve-only edge (Calls/Uses/Unresolved*): {edges:?}"
                );
            }

            // node_modules is skipped: no File/symbol node is drawn from it.
            assert!(
                files.iter().all(|f| !f.contains("/node_modules/")),
                "node_modules must not be scanned: {files:?}"
            );
            assert!(
                records.iter().all(|r| {
                    !r.get("path")
                        .and_then(|p| p.as_str())
                        .is_some_and(|p| p.contains("/node_modules/"))
                }),
                "no symbol may be drawn from node_modules"
            );

            assert!(
                stderr.contains("Languages: js"),
                "a JS-only repo must auto-detect the js id: {stderr}"
            );
            let _ = std::fs::remove_dir_all(&base);
        }

        /// Phase-06 task-13 (e2e): a mixed JS/TS scratch repo scanned ONCE under
        /// the unified frontend (`ts` id). BOTH resolution directions resolve to
        /// the real declared project symbol (never Unresolved): a `.js` import of
        /// a `.ts` definition and a `.ts` import of a `.js` definition. Also
        /// `package.json` `main`/`exports` self-name resolution and `.d.ts`
        /// declaration handling.
        #[test]
        #[ignore = "e2e tier: real I/O (scratch /tmp repo scanned by the candidate binary); run via cargo test-e2e"]
        fn mixed_js_ts_repo_resolves_both_directions() {
            assert!(ts_frontend_artifact().is_file());
            let (base, repo_dir, home) = js_scratch("mixed-js-ts", &mixed_js_ts_fixture());
            let scan = winb_run(&repo_dir, &home, &["scan", "."]);
            let stderr = String::from_utf8_lossy(&scan.stderr).to_string();
            assert!(scan.status.success(), "scan failed: {stderr}");
            // One frontend, one id: a TS-detected mixed repo never runs js+ts.
            assert!(
                stderr.contains("Languages: ts"),
                "a mixed repo scans once under ts: {stderr}"
            );
            assert!(
                !stderr.contains("Languages: js"),
                "js must be suppressed when ts is present: {stderr}"
            );

            let records = export_records(&repo_dir);
            let symbols = export_symbol_fqns(&records);
            assert!(
                records.iter().any(|r| {
                    r.get("type").and_then(|t| t.as_str()) == Some("calls")
                        && r.get("from").and_then(|f| f.as_str())
                            == Some("ts.mixed.src.jsconsumer.useTs")
                        && r.get("to").and_then(|t| t.as_str())
                            == Some("ts.mixed.src.tsdef.tsHelper")
                }),
                "direction A: a .js import must resolve to its .ts definition (a real FQN, not \
                 Unresolved): {records:?}"
            );
            assert!(
                records.iter().any(|r| {
                    r.get("type").and_then(|t| t.as_str()) == Some("calls")
                        && r.get("from").and_then(|f| f.as_str())
                            == Some("ts.mixed.src.tsconsumer.useJs")
                        && r.get("to").and_then(|t| t.as_str())
                            == Some("ts.mixed.src.jsdef.jsHelper")
                }),
                "direction B: a .ts import must resolve to its .js definition (a real FQN, not \
                 Unresolved): {records:?}"
            );
            assert!(symbols.contains("ts.mixed.src.tsdef.tsHelper"));
            assert!(symbols.contains("ts.mixed.src.jsdef.jsHelper"));

            // package.json `main`/`exports`: the self-name import reaches the
            // declared `src/index.js` symbol (a real resolved target).
            assert!(
                records.iter().any(|r| {
                    r.get("type").and_then(|t| t.as_str()) == Some("calls")
                        && r.get("from").and_then(|f| f.as_str())
                            == Some("ts.mixed.src.selfconsumer.useIndex")
                        && r.get("to").and_then(|t| t.as_str())
                            == Some("ts.mixed.src.index.indexHelper")
                }),
                "package.json main/exports resolution must reach the declared symbol: {records:?}"
            );

            // A JS workspace package resolved by NAME exercises the
            // `workspaceHost` JS-candidate path (task-22's extension: a `.js`
            // `index` candidate resolves without a build step). fix-module-
            // identity phase-02: `collectSources` skips the nested discovered-
            // package directory, so the workspace package's file is collected
            // under the workspace package ONLY — it is never re-emitted under
            // the recursive root package
            // (`requirements.constraint.no-checkout-named-module`).
            let lib_symbols: BTreeSet<String> = symbols
                .iter()
                .filter(|f| f.ends_with(".index.libHelper"))
                .cloned()
                .collect();
            assert_eq!(
                lib_symbols,
                BTreeSet::from(["ts.mixed-lib.index.libHelper".to_string()]),
                "a nested workspace file must be emitted under exactly one module identity: {symbols:?}"
            );
            assert!(
                !symbols.iter().any(|f| f.starts_with("ts.mixed.lib.")),
                "the nested package's files must not be re-emitted under the recursive root: {symbols:?}"
            );
            // The nested package's File node is contained by exactly one module.
            let lib_file = export_file_ending(&records, "/lib/index.js")
                .expect("lib/index.js must be a File node");
            let lib_parents: BTreeSet<String> = records
                .iter()
                .filter(|r| r.get("type").and_then(|t| t.as_str()) == Some("contains"))
                .filter(|r| r.get("to").and_then(|t| t.as_str()) == Some(lib_file.as_str()))
                .filter_map(|r| r.get("from").and_then(|f| f.as_str()).map(str::to_string))
                .collect();
            assert_eq!(
                lib_parents,
                BTreeSet::from(["ts.mixed-lib".to_string()]),
                "lib/index.js must be contained by exactly one module identity: {lib_parents:?}"
            );
            assert!(
                records.iter().any(|r| {
                    r.get("type").and_then(|t| t.as_str()) == Some("calls")
                        && r.get("from").and_then(|f| f.as_str())
                            == Some("ts.mixed.src.selfconsumer.useLib")
                        && r.get("to")
                            .and_then(|t| t.as_str())
                            .is_some_and(|t| lib_symbols.contains(t))
                }),
                "a named JS workspace import must resolve through the JS candidate path: {records:?}"
            );

            // `.d.ts` handling: the declaration is a File node and its interface
            // is a project symbol used by the `.ts` consumer.
            assert!(
                export_file_ending(&records, "/src/types.d.ts").is_some(),
                "the .d.ts declaration file must be scanned"
            );
            assert!(
                symbols.contains("ts.mixed.src.types.Config"),
                "the .d.ts interface must be a project symbol: {symbols:?}"
            );
            assert!(
                export_resolved_edge_targets(&records).contains("ts.mixed.src.types.Config"),
                "the .d.ts interface must be a resolved uses target: {records:?}"
            );

            let _ = std::fs::remove_dir_all(&base);
        }

        /// Phase-06 task-14 (e2e): a scratch repo whose JavaScript uses dynamic
        /// `require(expr)` and computed member access. The never-fabricate rule
        /// holds: only `unresolved_call`/`unresolved_use` edges leave the
        /// fixture's functions, each unresolved target carries a category, and
        /// there are ZERO guessed `calls`/`uses` edges.
        #[test]
        #[ignore = "e2e tier: real I/O (scratch /tmp repo scanned by the candidate binary); run via cargo test-e2e"]
        fn dynamic_js_yields_only_categorised_unresolved() {
            assert!(ts_frontend_artifact().is_file());
            let (base, repo_dir, home) = js_scratch("dynamic-js", &dynamic_js_fixture());
            let scan = winb_run(&repo_dir, &home, &["scan", "."]);
            let stderr = String::from_utf8_lossy(&scan.stderr).to_string();
            assert!(scan.status.success(), "scan failed: {stderr}");

            let records = export_records(&repo_dir);
            let dyn_file = export_file_ending(&records, "/dynamic.js")
                .expect("dynamic.js must be a File node");
            let syms = export_symbols_at(&records, &dyn_file);
            assert!(!syms.is_empty(), "dynamic.js must declare symbols");
            let edges = export_edge_types_from(&records, &syms);
            assert!(
                !edges.is_empty(),
                "the dynamic constructs must emit an edge"
            );
            assert!(
                edges.iter().all(|t| t.starts_with("unresolved_")),
                "only unresolved edges may leave untyped dynamic JS, got {edges:?}"
            );

            let unresolved = export_unresolved(&records);
            assert!(
                !unresolved.is_empty(),
                "the dynamic constructs must be unresolved targets"
            );
            for (fqn, category) in &unresolved {
                assert!(
                    !category.is_empty(),
                    "unresolved target {fqn} must carry a category"
                );
            }

            let _ = std::fs::remove_dir_all(&base);
        }

        /// Phase-06 task-15 (e2e): the self-scan acceptance — the candidate's
        /// BUILT/STAGED unified frontend scans a scratch COPY of the ported
        /// `src/tslib` (`scanner.ts` + its `package.json`). The graph is not just
        /// an empty module node: the package module, a File node for scanner.ts,
        /// the ported declaration units, and at least one resolve-only edge.
        /// Never points the candidate at the real apg checkout.
        #[test]
        #[ignore = "e2e tier: real I/O (scratch /tmp repo scanned by the candidate binary); run via cargo test-e2e"]
        fn self_scan_of_ported_tslib_produces_symbols() {
            assert!(ts_frontend_artifact().is_file());
            let (base, repo_dir, home) = js_scratch_owned("self-scan", &tslib_self_scan_fixture());
            let scan = winb_run(&repo_dir, &home, &["scan", "."]);
            let stderr = String::from_utf8_lossy(&scan.stderr).to_string();
            assert!(scan.status.success(), "scan failed: {stderr}");

            let records = export_records(&repo_dir);
            assert!(
                export_modules(&records).contains("ts.apg-tsfrontend"),
                "the copied package module must be present: {:?}",
                export_modules(&records)
            );
            let scanner = export_file_ending(&records, "/tslib/scanner.ts")
                .expect("scanner.ts must be a File node");
            let syms = export_symbols_at(&records, &scanner);
            assert!(
                !syms.is_empty(),
                "scanner.ts must declare symbols, got none"
            );
            for unit in [
                "collectFile",
                "emitNode",
                "discoverPackages",
                "collectSources",
                "isSourceExt",
                "relPrefix",
                "workspaceHost",
                "emitEdge",
                "emitUnresolved",
                "registerStruct",
                "registerFunction",
                "handleCall",
                "handleNew",
                "handleType",
                "handleJsx",
                "walkNode",
            ] {
                let fqn = format!("ts.apg-tsfrontend.scanner.{unit}");
                assert!(
                    syms.contains(&fqn),
                    "ported unit {fqn} must be a symbol: {syms:?}"
                );
            }
            assert!(
                has_resolved_project_edge(&records),
                "the self-scan must emit at least one resolve-only edge"
            );

            let _ = std::fs::remove_dir_all(&base);
        }

        /// Phase-07 task-13 (e2e, scratch /tmp repo, CANDIDATE binary only —
        /// `global.constraint.no-real-project-test`): the Markdown frontend's
        /// pinned AC through ONE REAL mixed Go+Markdown scan. Asserts one Module
        /// per directory (FQN = `md.<repo-relative directory path>`), repo-relative
        /// path File nodes with the extension retained, nested section Structs
        /// with contains edges, injective duplicate-heading dedup, same-stem /
        /// path-alias distinctness, `.mdx` excluded, md auto-detected ALONGSIDE
        /// go, and the md `code_type` (`docs`/`generated`). `shadowed_modules ==
        /// 0` is asserted POSITIVELY on the frontend's own emitted records (the
        /// in-process ingest of its spool observes the counter; the scan's
        /// warning absence alone is vacuous), and the scan exits 0 with no
        /// `claim` same-kind panic.
        #[test]
        #[ignore = "e2e tier: real I/O (scratch /tmp repo scanned by the candidate binary); run via cargo test-e2e"]
        fn acceptance_markdown_scratch_modules_sections_and_code_types() {
            let (base, repo_dir, home) = md_scratch("acceptance", &md_mixed_fixture());

            // `apg init` writes a layout config whose `default` (`src`) REPLACES
            // the builtin per-language rules, so opt this fixture into the md
            // provenance classes (generated wins its `gen/` tree, ordinary
            // markdown is docs) — a real project config that keeps the version
            // field. The builtin md arm itself is asserted separately below on
            // the frontend's OWN records with no config.
            std::fs::write(
                repo_dir.join("apg/config.json"),
                format!(
                    "{{\n  \"default\": \"src\",\n  \"types\": [\n    \
                     {{ \"name\": \"generated\", \"globs\": [\"**/gen/**\"] }},\n    \
                     {{ \"name\": \"external\", \"globs\": [\"**/vendor/**\"] }},\n    \
                     {{ \"name\": \"docs\", \"globs\": [\"**/*.md\", \"**/*.markdown\"] }}\n  \
                     ],\n  \"version\": \"{}\"\n}}\n",
                    env!("CARGO_PKG_VERSION")
                ),
            )
            .unwrap();
            scratch_commit_all(&repo_dir, "md code-type rules");

            // ---- the REAL mixed scan: md is auto-detected ALONGSIDE go ----
            let scan = winb_run(&repo_dir, &home, &["scan", "."]);
            let stderr = String::from_utf8_lossy(&scan.stderr).to_string();
            assert!(scan.status.success(), "scan failed: {stderr}");
            assert!(
                stderr.contains("Languages: go, md"),
                "a mixed repo must auto-detect md alongside go: {stderr}"
            );
            assert!(
                stderr.contains("[scan] running go frontend"),
                "the go frontend must run (mixed repo): {stderr}"
            );
            assert!(
                stderr.contains("[scan] running md frontend"),
                "the md frontend must run: {stderr}"
            );
            // A same-kind `claim` collision aborts the scan non-zero (already
            // covered by `success`); no module/function may be shadowed either.
            assert!(
                !stderr.contains("shadowed"),
                "no module/function may be shadowed: {stderr}"
            );

            let recs = export_records(&repo_dir);
            let root = std::fs::canonicalize(&repo_dir).unwrap();
            // The graph stores repo-relative identities: a File's fqn is its
            // `/`-separated path under the git toplevel, and an md module
            // identity (the directory path) renders `md.<repo-relative dir>`.
            let at = |rel: &str| rel.to_string();
            let md_at = |rel: &str| format!("md.{}", at(rel));

            // ---- (1) one Module per directory, FQN = `md.<directory absolute
            // path>` (post-P9), each emitted exactly once ----
            let module_counts = export_module_counts(&recs);
            for dir in ["docs", "docs/deep", "guides", "notes", "gen"] {
                assert_eq!(
                    module_counts.get(&md_at(dir)).copied(),
                    Some(1),
                    "directory `{dir}` must yield exactly one Module `{}`: {module_counts:?}",
                    md_at(dir)
                );
            }
            // An `.mdx`-only directory is not a Module (and yields no node).
            assert!(
                !module_counts.contains_key(&md_at("onlymdx")),
                "an .mdx-only directory must not be a Module: {module_counts:?}"
            );
            // Positive no-double-emission check on the emitted records.
            for (fqn, n) in &module_counts {
                assert_eq!(*n, 1, "module `{fqn}` emitted {n} times");
            }

            // ---- (2) File nodes: absolute path, extension retained, with the
            // md code_type (`docs`; `generated` under gen/) ----
            let file_type: std::collections::BTreeMap<String, String> = recs
                .iter()
                .filter(|r| r.get("type").and_then(|t| t.as_str()) == Some("file"))
                .map(|r| {
                    (
                        r.get("fqn")
                            .and_then(|f| f.as_str())
                            .unwrap_or("")
                            .to_string(),
                        r.get("code_type")
                            .and_then(|c| c.as_str())
                            .unwrap_or("")
                            .to_string(),
                    )
                })
                .collect();
            for rel in [
                "docs/overview.md",
                "docs/nesting.md",
                "docs/empty.md",
                "docs/intro.md",
                "guides/intro.md",
                "docs/deep/page.md",
                "notes/README.md",
                "notes/README.markdown",
                "gen/generated.md",
            ] {
                let expected = if rel.starts_with("gen/") {
                    "generated"
                } else {
                    "docs"
                };
                assert_eq!(
                    file_type.get(&at(rel)).map(String::as_str),
                    Some(expected),
                    "md file `{rel}` must be a `{expected}` File node: {file_type:?}"
                );
            }
            // `.mdx` is NOT Markdown to the bundled scanner: it yields NO `md.*`
            // node (no md module, no heading Struct). It is a residual `misc`
            // File instead — the every-tracked-file-graphed design — so a `.mdx`
            // File node is classified `config`, and an `.mdx`-only directory
            // roots as `misc.<dir>`, never `md.<dir>`.
            let mdx_files: BTreeSet<String> = recs
                .iter()
                .filter(|r| r.get("type").and_then(|t| t.as_str()) == Some("file"))
                .filter_map(|r| r.get("fqn").and_then(|f| f.as_str()).map(str::to_string))
                .filter(|f| f.ends_with(".mdx"))
                .collect();
            for rel in ["notes/draft.mdx", "onlymdx/draft.mdx"] {
                assert!(
                    mdx_files.contains(&at(rel)),
                    "`{rel}` must be a residual `misc` File node: {mdx_files:?}"
                );
                assert_eq!(
                    file_type.get(&at(rel)).map(String::as_str),
                    Some("config"),
                    "the `.mdx` File `{rel}` must classify `config` (residual misc): {file_type:?}"
                );
            }
            // No `md.*` node is drawn from a `.mdx` file: an `.mdx`-only
            // directory never roots as `md.<dir>` (it roots as `misc.<dir>`),
            // and no heading Struct's FQN embeds a `.mdx` path.
            assert!(
                !module_counts.contains_key(&md_at("onlymdx")),
                "an .mdx-only directory must never root as `md.onlymdx`: {module_counts:?}"
            );
            assert_eq!(
                module_counts.get("misc.onlymdx").copied(),
                Some(1),
                "an .mdx-only directory must root as `misc.onlymdx`: {module_counts:?}"
            );
            assert!(
                !recs.iter().any(|r| {
                    r.get("type").and_then(|t| t.as_str()) == Some("struct")
                        && r.get("fqn")
                            .and_then(|f| f.as_str())
                            .is_some_and(|f| f.starts_with("md.") && f.contains(".mdx"))
                }),
                "no `md.*` heading Struct may be drawn from an .mdx file"
            );

            let struct_fqns: BTreeSet<String> = recs
                .iter()
                .filter(|r| r.get("type").and_then(|t| t.as_str()) == Some("struct"))
                .filter_map(|r| r.get("fqn").and_then(|f| f.as_str()).map(str::to_string))
                .collect();
            let contains: BTreeSet<(String, String)> = recs
                .iter()
                .filter(|r| r.get("type").and_then(|t| t.as_str()) == Some("contains"))
                .map(|r| {
                    (
                        r.get("from")
                            .and_then(|f| f.as_str())
                            .unwrap_or("")
                            .to_string(),
                        r.get("to")
                            .and_then(|t| t.as_str())
                            .unwrap_or("")
                            .to_string(),
                    )
                })
                .collect();

            // ---- (3) nested sections + the pinned duplicate-heading dedup:
            // Overview/Overview/Overview 1 -> overview/overview-1/overview-2,
            // each FQN extending its parent (`<File>.<slug>`, then the parent
            // section's FQN). ----
            // PHASE_09: a top-level section's parent is the File path, which the
            // ingestor roots via `rooted_scope`, so every section FQN is
            // `md.<file-path>.<slug>` (the slug chain then extends it). The
            // File→section `contains` edge keeps the File's OWN (unrooted) path
            // as its `from` (ingest Pass B3 derives it from the node's location).
            let ov = at("docs/overview.md");
            let o1 = format!("md.{ov}.overview");
            let o2 = format!("md.{ov}.overview.overview-1");
            let o3 = format!("md.{ov}.overview.overview-1.overview-2");
            for f in [&o1, &o2, &o3] {
                assert!(
                    struct_fqns.contains(f),
                    "duplicate-heading dedup must render `{f}`: {struct_fqns:?}"
                );
            }

            // The second `##` is a sibling of the first (`title`); the file's
            // top-level section hangs off the File; a sibling after a deeper
            // heading attaches to the nearest preceding LOWER-level heading.
            let ns = at("docs/nesting.md");
            let n1 = format!("md.{ns}.title");
            let n2 = format!("md.{ns}.title.child");
            let n3 = format!("md.{ns}.title.child.grandchild");
            let n4 = format!("md.{ns}.title.sibling");
            for f in [&n1, &n2, &n3, &n4] {
                assert!(
                    struct_fqns.contains(f),
                    "nesting must render `{f}`: {struct_fqns:?}"
                );
            }

            for (from, to) in [
                (ov.clone(), o1.clone()),
                (o1.clone(), o2.clone()),
                (o2.clone(), o3.clone()),
                (ns.clone(), n1.clone()),
                (n1.clone(), n2.clone()),
                (n2.clone(), n3.clone()),
                (n1.clone(), n4.clone()),
            ] {
                assert!(
                    contains.contains(&(from.clone(), to.clone())),
                    "contains `{from} -> {to}` must hold: {contains:?}"
                );
            }

            // ---- (4) a heading-less document yields NO section Struct — only
            // its File node; no synthesized document-root anchors it. ----
            let empty = at("docs/empty.md");
            assert!(
                export_symbols_at(&recs, &empty).is_empty(),
                "a heading-less document must declare NO section Struct: {:?}",
                export_symbols_at(&recs, &empty)
            );
            assert!(
                !struct_fqns.contains(&empty),
                "no synthesized document-root Struct for a heading-less file"
            );

            // ---- (5) same-stem files in ONE directory share exactly one
            // Module but stay TWO distinct File nodes (extension retained). ----
            let readme_md = at("notes/README.md");
            let readme_markdown = at("notes/README.markdown");
            assert_eq!(
                module_counts.get(&md_at("notes")).copied(),
                Some(1),
                "the same-dir markdown files must share ONE Module"
            );
            assert!(
                file_type.contains_key(&readme_md) && file_type.contains_key(&readme_markdown),
                "README.md + README.markdown must be two distinct File nodes: {file_type:?}"
            );
            assert!(
                struct_fqns.contains(&format!("md.{readme_md}.notes"))
                    && struct_fqns.contains(&format!("md.{readme_markdown}.notes")),
                "same-stem sections must stay distinct by file path: {struct_fqns:?}"
            );

            // ---- (6) path-alias / same-stem-across-directories stay distinct,
            // and a subdirectory is its own Module (not an alias of `docs`). ----
            let docs_intro = at("docs/intro.md");
            let guides_intro = at("guides/intro.md");
            assert_ne!(docs_intro, guides_intro);
            assert_ne!(at("docs"), at("docs/deep"));
            assert!(
                file_type.contains_key(&docs_intro)
                    && file_type.contains_key(&guides_intro)
                    && module_counts.contains_key(&md_at("docs/deep")),
                "same-stem files in different directories must stay distinct: {file_type:?}"
            );
            assert!(
                struct_fqns.contains(&format!("md.{docs_intro}.intro"))
                    && struct_fqns.contains(&format!("md.{guides_intro}.intro")),
                "same-stem sections across directories must be distinct: {struct_fqns:?}"
            );

            // The requirement's path-alias fixture: `docs/guide/x.md` and
            // `docs.guide/x.md` must NOT alias (dot-joining the directory path
            // would collapse both to one `docs.guide` prefix).
            let guide_x = at("docs/guide/x.md");
            let dotguide_x = at("docs.guide/x.md");
            assert_eq!(module_counts.get(&md_at("docs/guide")).copied(), Some(1));
            assert_eq!(module_counts.get(&md_at("docs.guide")).copied(), Some(1));
            assert_ne!(at("docs/guide"), at("docs.guide"));
            assert!(
                file_type.contains_key(&guide_x) && file_type.contains_key(&dotguide_x),
                "the path-alias files must stay distinct File nodes: {file_type:?}"
            );
            assert!(
                struct_fqns.contains(&format!("md.{guide_x}.x"))
                    && struct_fqns.contains(&format!("md.{dotguide_x}.x")),
                "the path-alias sections must stay distinct: {struct_fqns:?}"
            );

            // ---- (7) the BUILTIN md code-type arm (no config) and
            // shadowed_modules == 0 POSITIVELY: ingest the frontend's own
            // emitted records in-process and observe both the classifier and
            // the counter (the real scan's warning absence alone is vacuous,
            // and an init layout's config default replaces the builtin rules,
            // so this is where the builtin arm is exercised). ----
            let md_records = md_frontend_records(&repo_dir);
            let (md_graph, report) = crate::ingest::ingest(
                md_records,
                &crate::ingest::IngestOptions {
                    blacklist: &[],
                    language: "md",
                    config: None,
                    base: Some(&root),
                },
            );
            for rel in [
                "docs/overview.md",
                "docs/nesting.md",
                "docs/empty.md",
                "docs/intro.md",
                "guides/intro.md",
                "docs/deep/page.md",
                "notes/README.md",
                "notes/README.markdown",
            ] {
                assert_eq!(
                    md_graph.nodes.get(&at(rel)).map(|n| n.code_type.as_str()),
                    Some("docs"),
                    "builtin md rule: ordinary markdown `{rel}` must classify docs"
                );
            }
            // `gen/` wins (generated) and the file stays in the graph
            // (all-code-included: filtered by code_type, never omitted).
            assert_eq!(
                md_graph
                    .nodes
                    .get(&at("gen/generated.md"))
                    .map(|n| n.code_type.as_str()),
                Some("generated"),
                "builtin md rule: a gen/ document must classify generated"
            );
            assert_eq!(
                report.shadowed_modules, 0,
                "the md records (duplicate-heading fixtures included) must shadow no module"
            );
            assert_eq!(
                report.shadowed_functions, 0,
                "the md records must shadow no function"
            );

            let _ = std::fs::remove_dir_all(&base);
        }

        /// Phase-08 task-17 (e2e, scratch /tmp repo, CANDIDATE binary only —
        /// `global.constraint.no-real-project-test`): the Python acceptance
        /// fixture suite through ONE real scan. Module IDENTITY is asserted
        /// ROOTING-AGNOSTICALLY (a leading `py.`/`python.` root is normalised
        /// away, so the same assertions hold pre- and post-P9). `.py` and `.pyi`
        /// each yield a File node parented to their module; a `.pyx` and every
        /// `site-packages` file yield NO code node; `*_test.py` is `test` and a
        /// `third_party/` file is `external` (asserted on both the real scan's
        /// config-opted export AND the frontend's own records under the builtin
        /// arm); `shadowed_modules == 0` and no same-kind `claim` panic.
        #[test]
        #[ignore = "e2e tier: real I/O (scratch /tmp repo scanned by the candidate binary); run via cargo test-e2e"]
        fn acceptance_python_scratch_modules_files_and_code_types() {
            let (base, repo_dir, home) = py_scratch("acceptance", &py_acceptance_fixture());
            write_py_config(&repo_dir);
            scratch_commit_all(&repo_dir, "py code-type rules");

            let scan = winb_run(&repo_dir, &home, &["scan", "."]);
            let stderr = String::from_utf8_lossy(&scan.stderr).to_string();
            assert!(scan.status.success(), "scan failed: {stderr}");
            assert!(
                stderr.contains("Languages: py"),
                "a Python repo must auto-detect py: {stderr}"
            );
            assert!(
                stderr.contains("[scan] running py frontend"),
                "the py frontend must run: {stderr}"
            );
            assert!(
                !stderr.contains("shadowed"),
                "no module/function may be shadowed: {stderr}"
            );

            let recs = export_records(&repo_dir);
            let root = std::fs::canonicalize(&repo_dir).unwrap();
            // The graph stores repo-relative identities: a `.py` File's fqn is
            // its `/`-separated path under the git toplevel.
            let at = |rel: &str| rel.to_string();

            // (1) Module identities, rooting-agnostic.
            let modules: BTreeSet<String> = export_modules(&recs)
                .iter()
                .map(|m| strip_lang_prefix(m))
                .collect();
            for m in ["pkg", "pkg.sub", "pkg.sub.mod", "pkg.sub.api", "foo", "bar"] {
                assert!(
                    modules.contains(m),
                    "module `{m}` must be present: {modules:?}"
                );
            }

            // (2) `.py` and `.pyi` File nodes, each parented to its module.
            // Containment is a `contains` edge (the export's File record has no
            // `parent` field), so the File's parent is the `from` of the
            // `module -> file path` edge.
            let parent_of = |path: &str| -> Option<String> {
                recs.iter()
                    .find(|r| {
                        r.get("type").and_then(|t| t.as_str()) == Some("contains")
                            && r.get("to").and_then(|t| t.as_str()) == Some(path)
                    })
                    .and_then(|r| {
                        r.get("from")
                            .and_then(|f| f.as_str())
                            .map(strip_lang_prefix)
                    })
            };
            assert_eq!(
                parent_of(&at("pkg/sub/mod.py")).as_deref(),
                Some("pkg.sub.mod"),
                "a .py File node must be parented to its module"
            );
            assert_eq!(
                parent_of(&at("pkg/sub/api.pyi")).as_deref(),
                Some("pkg.sub.api"),
                "a .pyi File node must be parented to its module"
            );

            // (3) Code types (the fixture's opt-in config).
            let types: std::collections::BTreeMap<String, String> = recs
                .iter()
                .filter(|r| r.get("type").and_then(|t| t.as_str()) == Some("file"))
                .filter_map(|r| {
                    Some((
                        r.get("fqn")?.as_str()?.to_string(),
                        r.get("code_type")?.as_str()?.to_string(),
                    ))
                })
                .collect();
            assert_eq!(
                types.get(&at("pkg/lifecycle_test.py")).map(String::as_str),
                Some("test"),
                "`*_test.py` must classify test: {types:?}"
            );
            assert_eq!(
                types.get(&at("third_party/lib/dep.py")).map(String::as_str),
                Some("external"),
                "a `third_party/` file must classify external: {types:?}"
            );

            // (4) `.pyx` and `site-packages` yield NO PYTHON code node. The
            // bundled structural scanner legitimately mints residual `misc`
            // File nodes for the tracked files it claims (e.g. `cy/mod.pyx`,
            // `.gitignore`) — every tracked file is graphed — so scope the
            // check to the Python stream: a py File carries its Python
            // code_type, while a structural File classifies `config`. The
            // Python code-node assertions stay intact.
            let py_locations: Vec<String> = recs
                .iter()
                .filter_map(|r| {
                    let ty = r.get("type").and_then(|t| t.as_str())?;
                    let path = match ty {
                        "file" => {
                            // Subtract the structural `misc`/format File facts.
                            if r.get("code_type").and_then(|c| c.as_str()) == Some("config") {
                                return None;
                            }
                            r.get("fqn").and_then(|f| f.as_str())
                        }
                        "struct" | "function" => r.get("path").and_then(|p| p.as_str()),
                        _ => None,
                    }?;
                    Some(path.to_string())
                })
                .collect();
            assert!(
                !py_locations.iter().any(|p| p.ends_with(".pyx")),
                "a `.pyx` must never become a Python code node: {py_locations:?}"
            );
            assert!(
                !py_locations.iter().any(|p| p.contains("site-packages")),
                "a `site-packages` tree must never become a Python code node: {py_locations:?}"
            );

            // (5) The BUILTIN py arm + shadow counters, observed POSITIVELY on
            // the frontend's own records (config-free).
            let front = py_frontend_records(&repo_dir);
            let (graph, report) = crate::ingest::ingest(
                front,
                &crate::ingest::IngestOptions {
                    blacklist: &[],
                    language: "py",
                    config: None,
                    base: Some(&root),
                },
            );
            assert_eq!(report.shadowed_modules, 0, "no py module may shadow");
            assert_eq!(report.shadowed_functions, 0, "no py function may shadow");
            assert_eq!(
                graph
                    .nodes
                    .get(&at("pkg/lifecycle_test.py"))
                    .map(|n| n.code_type.as_str()),
                Some("test"),
                "builtin py rule: `*_test.py` must classify test"
            );
            assert_eq!(
                graph
                    .nodes
                    .get(&at("third_party/lib/dep.py"))
                    .map(|n| n.code_type.as_str()),
                Some("external"),
                "builtin py rule: a `third_party/` file must classify external"
            );

            let _ = std::fs::remove_dir_all(&base);
        }

        /// Phase-08 task-18 (e2e): py resolution + unresolved classification
        /// through one real scan. A cross-file call is a `Calls` edge; a
        /// cross-file base class and a project-class constructor call are `Uses`
        /// to the base/class Struct; a stdlib reference is `stdlib` (never
        /// external); a hand-built `.venv` dependency is `external` (never
        /// stdlib); no guessed calls/uses edge is emitted.
        ///
        /// `unknown` precedence is NOT asserted here: the release pyfrontend the
        /// gate stages emits no base edge for the fixture's module-level alias
        /// base, so the in-root `unknown` case is unreachable through a real
        /// scan (it is unit-covered in `src/pylib`'s
        /// `unresolved_category_precedence_holds`). See the inline note.
        #[test]
        #[ignore = "e2e tier: real I/O (scratch /tmp repo scanned by the candidate binary); run via cargo test-e2e"]
        fn acceptance_python_resolution_and_unresolved_classification() {
            let (base, repo_dir, home) = py_scratch("resolve", &py_resolution_fixture());

            let scan = winb_run(&repo_dir, &home, &["scan", "."]);
            let stderr = String::from_utf8_lossy(&scan.stderr).to_string();
            assert!(scan.status.success(), "scan failed: {stderr}");

            let recs = export_records(&repo_dir);
            assert!(
                has_edge_between(&recs, "calls", "pkg.a.call", "pkg.b.helper"),
                "a.py calling b.py's function must be a Calls edge: {recs:?}"
            );
            assert!(
                has_edge_between(&recs, "uses", "pkg.a.User", "pkg.base.Base"),
                "a cross-file base class must be a Uses edge to the base Struct: {recs:?}"
            );
            assert!(
                has_edge_between(&recs, "uses", "pkg.a.make", "pkg.base.Base"),
                "a constructor call on a project class must be a Uses edge: {recs:?}"
            );

            let unresolved = export_unresolved(&recs);
            assert!(
                unresolved.contains(&("os.getcwd".to_string(), "stdlib".to_string())),
                "a stdlib/typeshed reference must be stdlib: {unresolved:?}"
            );
            assert!(
                unresolved.iter().any(|(_, c)| c == "external"),
                "a venv dependency reference must be external: {unresolved:?}"
            );
            // The fixture's in-root alias base (`Aliased(BaseAlias)`) is
            // unresolved in the RELEASE pyfrontend the gate stages
            // (`build.rs` builds `src/pylib` `--release`): `goto_definition`
            // does not follow the module-level `BaseAlias = Base` alias, so no
            // base edge is emitted at all rather than an `unknown`
            // unresolved_use. The `unknown` classification that path would
            // carry is unit-covered in the py frontend (`classify_unresolved`,
            // `unresolved_category_precedence_holds`); this acceptance keeps the
            // two reachable classifications and the no-bleed rule below.
            // Precedence: a category never bleeds across the classes.
            assert!(
                !unresolved
                    .iter()
                    .any(|(f, c)| f.contains("os.") && c == "external"),
                "a stdlib reference must never be external: {unresolved:?}"
            );
            assert!(
                !unresolved
                    .iter()
                    .any(|(f, c)| f.starts_with("dep") && c == "stdlib"),
                "a venv dependency must never be stdlib: {unresolved:?}"
            );

            // No guessed calls/uses edge leaves the in-root dynamic reference:
            // the alias base must never be fabricated into a `Uses` edge.
            assert!(
                !recs.iter().any(|r| {
                    r.get("type").and_then(|t| t.as_str()) == Some("uses")
                        && r.get("from")
                            .and_then(|f| f.as_str())
                            .is_some_and(|f| strip_lang_prefix(f) == "pkg.a.Aliased")
                }),
                "the dynamic base must not be guessed into a Uses edge: {recs:?}"
            );

            let _ = std::fs::remove_dir_all(&base);
        }

        /// Phase-08 task-19 (e2e): the incremental contract (the environment
        /// model is the sibling
        /// `acceptance_python_environment_model_and_external_resolution`). Each
        /// scenario runs on its OWN fresh scratch repo so the delta contains
        /// exactly the one file it mutates, mirroring the Go acceptance
        /// scenarios. A targeted re-scan of an edited CALLER whose call target
        /// lives in an unchanged, un-emitted module must NOT drop the caller:
        /// the changed file and its resolved `Calls` edge survive. Editing the
        /// TARGET (a signature change) re-emits its dependents. In every case
        /// the incremental Python graph equals a fresh full scan exactly (node
        /// set, edge set, unresolved-target set).
        #[test]
        #[ignore = "e2e tier: real I/O (scratch /tmp repo scanned by the candidate binary); run via cargo test-e2e"]
        #[allow(clippy::type_complexity)]
        fn acceptance_python_incremental_graph_equivalence() {
            // (tag, mutate, verdict assertion)
            let scenarios: Vec<(&str, fn(&Path), fn(&str))> = vec![
                (
                    "caller-edit",
                    |repo| {
                        std::fs::write(
                            repo.join("pkg/a.py"),
                            "from pkg.b import helper\n\n\ndef call() -> int:\n    return helper() + 1\n",
                        )
                        .unwrap();
                    },
                    |err| {
                        assert!(
                            !err.contains("signature change cascades"),
                            "a body-only caller edit must not cascade: {err}"
                        )
                    },
                ),
                (
                    "target-signature",
                    |repo| {
                        std::fs::write(
                            repo.join("pkg/b.py"),
                            "def helper() -> int:\n    return 1\n\n\ndef helper2() -> int:\n    return 2\n",
                        )
                        .unwrap();
                    },
                    |err| {
                        assert!(
                            err.contains("signature change cascades"),
                            "a target signature change must re-emit its dependents: {err}"
                        )
                    },
                ),
            ];

            for (tag, mutate, check) in scenarios {
                let (base, repo_dir) = py_incremental_scratch(tag, &py_incremental_fixture());
                let home = base.join("home");
                let init = winb_run(&repo_dir, &home, &["init", "."]);
                assert!(
                    init.status.success(),
                    "{tag}: apg init: {}",
                    String::from_utf8_lossy(&init.stderr)
                );
                scratch_commit_all(&repo_dir, "apg init");

                let cold = winb_run(&repo_dir, &home, &["scan", "."]);
                assert!(
                    cold.status.success(),
                    "{tag}: cold scan: {}",
                    String::from_utf8_lossy(&cold.stderr)
                );

                mutate(&repo_dir);

                let inc = winb_run(&repo_dir, &home, &["scan", "."]);
                let inc_err = String::from_utf8_lossy(&inc.stderr).to_string();
                assert!(inc.status.success(), "{tag}: incremental scan: {inc_err}");
                assert!(
                    inc_err.contains("incremental:"),
                    "{tag}: the incremental verdict must be printed: {inc_err}"
                );
                check(&inc_err);

                // The changed caller must not vanish: its resolved `Calls` edge
                // into the unchanged, un-emitted module survives.
                let recs = export_records(&repo_dir);
                assert!(
                    has_edge_between(&recs, "calls", "pkg.a.call", "pkg.b.helper"),
                    "{tag}: a re-scanned caller's call into the unchanged module must \
                     survive: {recs:?}\n--- stderr ---\n{inc_err}"
                );
                let (inc_nodes, inc_edges, inc_unres) = winb_graph(&repo_dir);

                // Oracle: a fresh FULL scan with the DB, the export, AND the
                // shared fact cache cleared — no reuse/splice.
                std::fs::remove_file(repo_dir.join("apg/.trans/db.lbug")).unwrap();
                std::fs::remove_file(repo_dir.join("apg/.trans/graph.jsonl")).unwrap();
                let _ = std::fs::remove_dir_all(repo_dir.join(".git/apg/facts"));
                let full = winb_run(&repo_dir, &home, &["scan", "."]);
                assert!(
                    full.status.success(),
                    "{tag}: full scan oracle: {}",
                    String::from_utf8_lossy(&full.stderr)
                );
                let (full_nodes, full_edges, full_unres) = winb_graph(&repo_dir);
                assert_eq!(
                    inc_nodes, full_nodes,
                    "{tag}: node sets must be exactly equal (incremental vs full)\n{inc_err}"
                );
                assert_eq!(
                    inc_edges, full_edges,
                    "{tag}: edge sets must be exactly equal (incremental vs full)\n{inc_err}"
                );
                assert_eq!(
                    inc_unres, full_unres,
                    "{tag}: unresolved-target sets must be exactly equal\n{inc_err}"
                );

                let _ = std::fs::remove_dir_all(&base);
            }
        }

        /// Phase-08 task-19 (e2e, FIXTURE MODEL feedback-83): the Python
        /// environment model is HAND-BUILT inside a scratch dir — a `.venv`
        /// site-packages stub tree plus `pyproject.toml`/`uv.lock`/`pyvenv.cfg`
        /// markers — and every frontend run has an EMPTY `PATH`, so ty resolves
        /// it WITHOUT shelling out to `python`/`uv` and no interpreter is
        /// required. A uv project / virtualenv / bare src root auto-detects, and
        /// a third-party import resolves against the real environment as
        /// `external` (never stdlib).
        #[test]
        #[ignore = "e2e tier: real I/O (temp dir fs + spawned pyfrontend); run via cargo test-e2e"]
        fn acceptance_python_environment_model_and_external_resolution() {
            let base = std::env::temp_dir().join(format!("apg-pyenv-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&base);
            let write = |rel: &str, body: &str| {
                let p = base.join(rel);
                std::fs::create_dir_all(p.parent().unwrap()).unwrap();
                std::fs::write(&p, body).unwrap();
            };

            // uv project: pyproject.toml + uv.lock markers.
            write(
                "uv/pyproject.toml",
                "[project]\nname = \"uvproj\"\nversion = \"0.1.0\"\n",
            );
            write("uv/uv.lock", "version = 1\n");
            write("uv/app.py", "def f() -> int:\n    return 1\n");

            // virtualenv: a `.venv/pyvenv.cfg` marker and a stub site-packages
            // tree, with NO pyproject.toml (a pure virtualenv project).
            write(
                "venv/.venv/pyvenv.cfg",
                "home = /nonexistent\nversion = 3.12.0\n",
            );
            write(
                "venv/.venv/lib/python3.12/site-packages/dep/__init__.py",
                "def thing() -> int:\n    return 1\n",
            );
            write("venv/app.py", "def f() -> int:\n    return 1\n");

            // bare src root: no markers at all.
            write("bare/app.py", "def g() -> int:\n    return 1\n");

            // A dependency environment: pyproject.toml + `.venv` site-packages
            // with a third-party import (ty's project discovery adopts the
            // `.venv` through the pyproject marker).
            write(
                "dep/pyproject.toml",
                "[project]\nname = \"depproj\"\nversion = \"0.1.0\"\nrequires-python = \">=3.12\"\n",
            );
            write(
                "dep/.venv/pyvenv.cfg",
                "home = /nonexistent\nversion = 3.12.0\n",
            );
            write(
                "dep/.venv/lib/python3.12/site-packages/dep/__init__.py",
                "def thing() -> int:\n    return 1\n",
            );
            write(
                "dep/app.py",
                "import dep\n\n\ndef use() -> int:\n    return dep.thing()\n",
            );

            let run = |dir: &Path| {
                std::process::Command::new(py_frontend_bin())
                    .arg(dir)
                    .env("PATH", "")
                    .output()
                    .unwrap_or_else(|e| panic!("spawn pyfrontend: {e}"))
            };

            for (dir, kind) in [("uv", "Uv"), ("venv", "VirtualEnv"), ("bare", "BareSrc")] {
                let out = run(&base.join(dir));
                let stderr = String::from_utf8_lossy(&out.stderr).to_string();
                assert!(out.status.success(), "{dir}: pyfrontend failed: {stderr}");
                assert!(
                    stderr.contains(&format!("{kind} project")),
                    "{dir}: expected a {kind} project from filesystem markers alone, \
                     with no interpreter: {stderr}"
                );
            }

            // A third-party import resolves against the hand-built environment
            // as external — never stdlib — with no Python runtime present.
            let records = py_frontend_records(&base.join("dep"));
            let unresolved: BTreeSet<(String, String)> = records
                .iter()
                .filter_map(|r| match r {
                    crate::schema::Record::Unresolved { fqn, category } => {
                        Some((fqn.clone(), category.clone().unwrap_or_default()))
                    }
                    _ => None,
                })
                .collect();
            assert!(
                unresolved.iter().any(|(_, c)| c == "external"),
                "a venv dependency must resolve external: {unresolved:?}"
            );
            assert!(
                unresolved.iter().all(|(_, c)| c != "stdlib"),
                "a venv dependency must never be stdlib: {unresolved:?}"
            );

            let _ = std::fs::remove_dir_all(&base);
        }

        /// Phase-08 task-20 (e2e): py language plumbing detection.
        /// `auto_detect_languages` selects `py` only for real `.py`/`.pyi`
        /// sources: a `.pyx`-only tree and a tree whose only Python lives under
        /// `site-packages`/`.venv`/`venv` never trigger it; and
        /// `available_languages` reports `py` only when a `pyfrontend` artifact
        /// is installed (driven through the real CLI with an isolated
        /// `APG_FRONTEND_DIR`).
        #[test]
        #[ignore = "e2e tier: real I/O (temp dir fs + spawned apg); run via cargo test-e2e"]
        fn python_language_plumbing_detection_and_availability() {
            let base = std::env::temp_dir().join(format!("apg-py-detect-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&base);
            let avail = vec!["py".to_string()];

            // Ordinary .py / .pyi detect py.
            std::fs::create_dir_all(base.join("plain")).unwrap();
            std::fs::write(base.join("plain/app.py"), "def f():\n    return 1\n").unwrap();
            assert_eq!(
                auto_detect_languages(&base.join("plain"), &avail),
                vec!["py".to_string()],
                "a .py tree must detect py"
            );
            std::fs::create_dir_all(base.join("stub")).unwrap();
            std::fs::write(base.join("stub/api.pyi"), "def f() -> int: ...\n").unwrap();
            assert_eq!(
                auto_detect_languages(&base.join("stub"), &avail),
                vec!["py".to_string()],
                "a .pyi-only tree must detect py"
            );

            // .pyx is NOT a candidate.
            std::fs::create_dir_all(base.join("pyx")).unwrap();
            std::fs::write(base.join("pyx/mod.pyx"), "def f():\n    pass\n").unwrap();
            assert!(
                auto_detect_languages(&base.join("pyx"), &avail).is_empty(),
                "a .pyx-only tree must not trigger py"
            );

            // Python under site-packages / .venv / venv never triggers py.
            for (dir, rel) in [
                ("sp", "site-packages/dep.py"),
                ("dotvenv", ".venv/lib/python3.12/site-packages/dep.py"),
                ("venvname", "venv/lib/python3.12/site-packages/dep.py"),
            ] {
                let p = base.join(dir).join(rel);
                std::fs::create_dir_all(p.parent().unwrap()).unwrap();
                std::fs::write(&p, "def dep():\n    return 1\n").unwrap();
                assert!(
                    auto_detect_languages(&base.join(dir), &avail).is_empty(),
                    "Python only under {rel} must not trigger py"
                );
            }

            // available_languages gates py on the installed pyfrontend. An
            // isolated frontends dir carrying only a stub `gofrontend` reports
            // no py (and never spawns pyfrontend); adding the REAL pyfrontend
            // makes py reportable and run.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;

                let (b, repo_dir, home) = py_scratch(
                    "avail",
                    &[
                        ("pkg/__init__.py", ""),
                        ("pkg/mod.py", "def f() -> int:\n    return 1\n"),
                    ],
                );
                let fe = b.join("frontends");
                std::fs::create_dir_all(&fe).unwrap();
                let go_stub = fe.join("gofrontend");
                std::fs::write(&go_stub, "#!/bin/sh\nexit 0\n").unwrap();
                std::fs::set_permissions(&go_stub, std::fs::Permissions::from_mode(0o755)).unwrap();

                let run = |dir: &Path, env: &Path| {
                    testutil::ApgCommand::new(&["scan", "."])
                        .cwd(dir)
                        .env("HOME", home.to_str().unwrap())
                        .env("APG_FRONTEND_DIR", env.to_str().unwrap())
                        .output()
                };

                let scan = run(&repo_dir, &fe);
                let out = String::from_utf8_lossy(&scan.stderr).to_string();
                assert!(scan.status.success(), "scan (go-only frontends): {out}");
                assert!(
                    !out.contains("Languages: py") && !out.contains("running py frontend"),
                    "py must NOT be reported/run when pyfrontend is absent: {out}"
                );

                // Force a re-scan (the fast-path would otherwise reuse the DB),
                // then install the real pyfrontend and re-run.
                let _ = std::fs::remove_file(repo_dir.join("apg/.trans/db.lbug"));
                let _ = std::fs::remove_file(repo_dir.join("apg/.trans/graph.jsonl"));
                let _ = std::fs::remove_dir_all(repo_dir.join(".git/apg/facts"));
                let real = testutil::apg_bin()
                    .parent()
                    .expect("apg binary has a parent")
                    .join("frontends");
                std::os::unix::fs::symlink(real.join("pyfrontend"), fe.join("pyfrontend")).unwrap();

                let scan2 = run(&repo_dir, &fe);
                let out2 = String::from_utf8_lossy(&scan2.stderr).to_string();
                assert!(scan2.status.success(), "scan (py installed): {out2}");
                assert!(
                    out2.contains("Languages: py") && out2.contains("running py frontend"),
                    "py must be reported/run once pyfrontend is installed: {out2}"
                );

                let _ = std::fs::remove_dir_all(&b);
            }

            let _ = std::fs::remove_dir_all(&base);
        }

        /// Phase-08 task-17/20 (e2e): a mixed Go + Python repo scans BOTH
        /// frontends into ONE graph — py auto-detects alongside go, both
        /// frontends run, the merged graph carries a py symbol and its resolved
        /// call edge with zero `shadowed_modules`, and the py frontend honours
        /// its `--id-prefix` opaque-id namespace (the per-language namespacing
        /// the scan relies on to merge two `n1`-starting streams).
        #[test]
        #[ignore = "e2e tier: real I/O (scratch /tmp repo scanned by the candidate binary); run via cargo test-e2e"]
        fn acceptance_python_mixed_language_single_graph() {
            let (base, repo_dir, home) = py_scratch(
                "mixed",
                &[
                    ("go.mod", "module scratch\n\ngo 1.21\n"),
                    ("main.go", "package main\n\nfunc main() {}\n"),
                    ("pkg/__init__.py", ""),
                    ("pkg/other.py", "def helper() -> int:\n    return 1\n"),
                    (
                        "pkg/mod.py",
                        "from pkg.other import helper\n\n\ndef call() -> int:\n    return helper()\n",
                    ),
                ],
            );

            let scan = winb_run(&repo_dir, &home, &["scan", "."]);
            let stderr = String::from_utf8_lossy(&scan.stderr).to_string();
            assert!(scan.status.success(), "scan failed: {stderr}");
            assert!(
                stderr.contains("Languages: go, py"),
                "a mixed repo must auto-detect both languages: {stderr}"
            );
            assert!(
                stderr.contains("[scan] running go frontend"),
                "the go frontend must run: {stderr}"
            );
            assert!(
                stderr.contains("[scan] running py frontend"),
                "the py frontend must run: {stderr}"
            );
            assert!(
                !stderr.contains("shadowed"),
                "no module/function may be shadowed in the merged graph: {stderr}"
            );

            let recs = export_records(&repo_dir);
            assert!(
                export_file_ending(&recs, "/main.go").is_some(),
                "the go file must be in the merged graph: {recs:?}"
            );
            let symbols = export_symbol_fqns(&recs);
            assert!(
                symbols
                    .iter()
                    .any(|s| strip_lang_prefix(s) == "pkg.mod.call"),
                "the py symbol must be in the merged graph: {symbols:?}"
            );
            assert!(
                has_edge_between(&recs, "calls", "pkg.mod.call", "pkg.other.helper"),
                "the py cross-module call must resolve in the merged graph: {recs:?}"
            );

            // The per-language opaque-id prefix: the scan merges two streams
            // whose ids both start at 1, so py must namespace under `py`.
            let py_records = py_frontend_records_with(&repo_dir, &["--id-prefix", "py"]);
            let py_ids: Vec<String> = py_records
                .iter()
                .filter_map(|r| match r {
                    crate::schema::Record::Struct { id, .. }
                    | crate::schema::Record::Function { id, .. } => Some(id.clone()),
                    _ => None,
                })
                .collect();
            assert!(!py_ids.is_empty(), "the py frontend must emit declarations");
            assert!(
                py_ids.iter().all(|id| id.starts_with("py")),
                "py opaque ids must carry the py prefix: {py_ids:?}"
            );

            let _ = std::fs::remove_dir_all(&base);
        }

        /// Phase-03 task-6: the scan-time toolchain preflight on the spawn path.
        /// A scratch `/tmp` git repo with an isolated frontends dir carrying a
        /// stub `gofrontend` (so `go` is selectable) and a stripped `PATH` (so
        /// the `go` tool is absent) must fail `apg scan` with the named,
        /// actionable error — no panic, and no silently-empty graph
        /// (`global.constraint.no-real-project-test`).
        #[test]
        #[ignore = "e2e tier: real I/O (scratch repo/spawned apg/PATH); run via cargo test-e2e"]
        #[cfg(unix)]
        fn scan_time_preflight_reports_missing_tool() {
            use std::os::unix::fs::PermissionsExt;

            let base = std::env::temp_dir().join(format!("apg-preflight-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&base);
            std::fs::create_dir_all(&base).unwrap();
            let home = base.join("home");
            std::fs::create_dir_all(home.join(".opencode/node_modules/@opencode-ai/plugin"))
                .unwrap();

            // A scratch git repo carrying a real Go source and a versioned `apg/`
            // layout at the binary's own version, so a scan reaches the frontend
            // spawn path rather than refusing on the layout gate.
            let repo = testutil::Repo::new("preflight");
            repo.write("main.go", "package main\n\nfunc main() {}\n");
            repo.commit_all("go source");

            // An isolated frontends dir holding ONLY a stub `gofrontend`: `go`
            // is selectable, but the real Go toolchain is not on the stripped
            // PATH below.
            let fe = base.join("frontends");
            std::fs::create_dir_all(&fe).unwrap();
            let go_stub = fe.join("gofrontend");
            std::fs::write(&go_stub, "#!/bin/sh\nexit 0\n").unwrap();
            std::fs::set_permissions(&go_stub, std::fs::Permissions::from_mode(0o755)).unwrap();

            // An empty directory as PATH: every required scan-time tool misses.
            let empty_path = base.join("empty-path");
            std::fs::create_dir_all(&empty_path).unwrap();

            let out = testutil::ApgCommand::new(&["scan", "--language", "go", "."])
                .cwd(&repo.root)
                .env("HOME", home.to_str().unwrap())
                .env("APG_FRONTEND_DIR", fe.to_str().unwrap())
                .env("PATH", empty_path.to_str().unwrap())
                .output();
            let stderr = String::from_utf8_lossy(&out.stderr).to_string();

            assert!(
                !out.status.success(),
                "a missing `go` tool must fail the scan, not skip the language: {stderr}"
            );
            assert!(
                stderr.contains("scan-time toolchain preflight failed"),
                "the failure must be the named preflight error: {stderr}"
            );
            assert!(
                stderr.contains("`go` frontend") && stderr.contains("required tool `go`"),
                "the error must name the language and the missing tool: {stderr}"
            );
            assert!(
                stderr.contains("brew install go"),
                "the error must carry the actionable install hint: {stderr}"
            );
            assert!(
                !stderr.contains("panicked") && !stderr.contains("Failed to run frontend"),
                "the missing tool must never surface as a cryptic panic: {stderr}"
            );
            // No empty graph: the scan bailed before writing one.
            assert!(
                !repo.root.join("apg/.trans/db.lbug").exists(),
                "a preflight failure must not leave an empty graph"
            );
            assert!(
                !repo.root.join("apg/.trans/graph.jsonl").exists(),
                "a preflight failure must not leave an empty graph export"
            );

            testutil::remove(&repo);
            let _ = std::fs::remove_dir_all(&base);
        }

        /// apg-0.17.0 phase-03 task-10 (e2e, scratch /tmp repo, CANDIDATE binary
        /// only — `global.constraint.no-real-project-test`): a REAL scan with the
        /// bundled structural scanner emits a File node for every tracked file it
        /// claims, while the code frontend keeps its own file.
        ///
        /// `build.rs` does not stage `structfrontend` until phase-04, so the
        /// test is self-sufficient: it builds the isolated `src/structlib` crate
        /// and stages its binary into a per-test frontends dir (with the
        /// profile's `gofrontend`) that the spawned scan resolves via
        /// `APG_FRONTEND_DIR` — the shared profile dir is never touched.
        #[test]
        #[ignore = "e2e tier: real I/O (scratch /tmp repo + staged structfrontend + spawned apg); run via cargo test-e2e"]
        fn structural_scan_emits_file_node_per_tracked_file() {
            let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
            let struct_bin = manifest_dir.join("src/structlib/target/debug/structfrontend");
            if !struct_bin.is_file() {
                let status = std::process::Command::new("cargo")
                    .args([
                        "build",
                        "--manifest-path",
                        "src/structlib/Cargo.toml",
                        "--bin",
                        "structfrontend",
                    ])
                    .current_dir(manifest_dir)
                    .status()
                    .expect("run cargo build for src/structlib");
                assert!(status.success(), "src/structlib must build");
            }
            assert!(
                struct_bin.is_file(),
                "structfrontend not built at {} — run: cargo build --manifest-path \
                 src/structlib/Cargo.toml --bin structfrontend",
                struct_bin.display()
            );

            let (base, repo_dir, home) = md_scratch(
                "structural-files",
                &[
                    ("go.mod", "module scratch\n\ngo 1.21\n"),
                    ("main.go", "package main\n\nfunc main() {}\n"),
                    ("run.sh", "#!/bin/sh\n\nhello() {\n  echo hi\n}\n"),
                    (
                        "ci.yaml",
                        "name: ci\njobs:\n  build:\n    steps:\n      - run: make\n",
                    ),
                    ("data.json", "{\n  \"alpha\": 1,\n  \"beta\": 2\n}\n"),
                    ("config.toml", "[package]\nname = \"x\"\n"),
                    ("LICENSE", "MIT\n"),
                ],
            );

            // A per-test frontends dir: the built structfrontend + the profile's
            // real gofrontend (the code frontend whose file the structural
            // scanner must NOT claim).
            let fe = base.join("frontends");
            std::fs::create_dir_all(&fe).unwrap();
            std::fs::copy(&struct_bin, fe.join("structfrontend")).unwrap();
            let profile_frontends = crate::testutil::apg_bin()
                .parent()
                .expect("apg binary has a parent")
                .join("frontends");
            let go = profile_frontends.join("gofrontend");
            assert!(
                go.is_file(),
                "gofrontend not staged at {} — build it first: cargo build",
                go.display()
            );
            std::fs::copy(&go, fe.join("gofrontend")).unwrap();

            let scan = testutil::ApgCommand::new(&["scan", "."])
                .cwd(&repo_dir)
                .env("HOME", home.to_str().unwrap())
                .env("APG_FRONTEND_DIR", fe.to_str().unwrap())
                .output();
            let stderr = String::from_utf8_lossy(&scan.stderr).to_string();
            assert!(scan.status.success(), "structural scan failed: {stderr}");
            assert!(
                stderr.contains("Languages: go, md"),
                "the code language + structural bundle must be selected: {stderr}"
            );
            assert!(
                stderr.contains("[scan] running go frontend")
                    && stderr.contains("[scan] running md frontend"),
                "both the go and the structural frontend must run: {stderr}"
            );

            let recs = export_records(&repo_dir);
            // (1) every tracked file the structural scanner claims has a File
            // node (the repo-relative path is the File FQN).
            let files = export_file_paths(&recs);
            for rel in [
                "run.sh",
                "ci.yaml",
                "data.json",
                "config.toml",
                "LICENSE",
                "main.go",
            ] {
                assert!(
                    files.contains(rel),
                    "`{rel}` must have a File node: {files:?}"
                );
            }
            // (2) the code frontend keeps its own file: `main.go` is classified
            // by the go frontend (`src`), not by the structural scanner
            // (`config`); the structural files are `config`.
            let file_type: std::collections::BTreeMap<String, String> = recs
                .iter()
                .filter(|r| r.get("type").and_then(|t| t.as_str()) == Some("file"))
                .map(|r| {
                    (
                        r.get("fqn")
                            .and_then(|f| f.as_str())
                            .unwrap_or("")
                            .to_string(),
                        r.get("code_type")
                            .and_then(|c| c.as_str())
                            .unwrap_or("")
                            .to_string(),
                    )
                })
                .collect();
            for rel in ["run.sh", "ci.yaml", "data.json", "config.toml", "LICENSE"] {
                assert_eq!(
                    file_type.get(rel).map(String::as_str),
                    Some("config"),
                    "structural file `{rel}` must classify config: {file_type:?}"
                );
            }
            assert_eq!(
                file_type.get("main.go").map(String::as_str),
                Some("src"),
                "the go frontend must keep `main.go` (code_type src): {file_type:?}"
            );
            // (3) the structural scanner declares structure for its formats.
            let symbols = export_symbol_fqns(&recs);
            assert!(
                symbols.contains("sh.run.sh.hello"),
                "the shell function must be a Struct: {symbols:?}"
            );
            assert!(
                symbols.contains("yaml.ci.yaml.name"),
                "the YAML top-level key must be a Struct: {symbols:?}"
            );
            assert!(
                symbols.contains("json.data.json.alpha"),
                "the JSON top-level key must be a Struct: {symbols:?}"
            );
            assert!(
                symbols.contains("toml.config.toml.package"),
                "the TOML table must be a Struct: {symbols:?}"
            );
            // (4) the residual `misc` stream emits a File only.
            assert!(
                export_symbols_at(&recs, "LICENSE").is_empty(),
                "a misc file must declare no Struct: {:?}",
                export_symbols_at(&recs, "LICENSE")
            );

            let _ = std::fs::remove_dir_all(&base);
        }

        /// apg-0.17.0 phase-03 task-11 (e2e, scratch /tmp repo, CANDIDATE binary
        /// only): a REAL scan with a root Markdown file renders the repo-root
        /// module repo-relative as `md.` (never the checkout basename), keeps
        /// the non-root `md.*` module and the heading Structs unchanged, and
        /// `md.` Contains the repo-root File. Self-sufficient staging (phase-04
        /// owns the build.rs staging), so the shared profile dir is untouched.
        #[test]
        #[ignore = "e2e tier: real I/O (scratch /tmp repo + staged structfrontend + spawned apg); run via cargo test-e2e"]
        fn structural_markdown_repo_root_renders_md_root() {
            let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
            let struct_bin = manifest_dir.join("src/structlib/target/debug/structfrontend");
            if !struct_bin.is_file() {
                let status = std::process::Command::new("cargo")
                    .args([
                        "build",
                        "--manifest-path",
                        "src/structlib/Cargo.toml",
                        "--bin",
                        "structfrontend",
                    ])
                    .current_dir(manifest_dir)
                    .status()
                    .expect("run cargo build for src/structlib");
                assert!(status.success(), "src/structlib must build");
            }
            assert!(
                struct_bin.is_file(),
                "structfrontend not built at {} — run: cargo build --manifest-path \
                 src/structlib/Cargo.toml --bin structfrontend",
                struct_bin.display()
            );

            let (base, repo_dir, home) = md_scratch(
                "structural-md-root",
                &[
                    ("README.md", "# Root Heading\n\nprose\n"),
                    ("docs/guide.md", "# Guide\n"),
                ],
            );

            let fe = base.join("frontends");
            std::fs::create_dir_all(&fe).unwrap();
            std::fs::copy(&struct_bin, fe.join("structfrontend")).unwrap();

            let scan = testutil::ApgCommand::new(&["scan", "."])
                .cwd(&repo_dir)
                .env("HOME", home.to_str().unwrap())
                .env("APG_FRONTEND_DIR", fe.to_str().unwrap())
                .output();
            let stderr = String::from_utf8_lossy(&scan.stderr).to_string();
            assert!(scan.status.success(), "markdown scan failed: {stderr}");
            assert!(
                stderr.contains("[scan] running md frontend"),
                "the structural md stream must run: {stderr}"
            );

            let recs = export_records(&repo_dir);
            // (1) the repo-root module renders the bare `md.` root, and the
            // non-root directory keeps its `md.*` identity.
            let modules = export_modules(&recs);
            assert!(
                modules.contains("md."),
                "the repo-root module must render `md.`: {modules:?}"
            );
            assert!(
                modules.contains("md.docs"),
                "the non-root dir module must render `md.docs`: {modules:?}"
            );
            // Never the checkout basename (the scratch repo dir is `repo`).
            let basename = repo_dir.file_name().unwrap().to_string_lossy().into_owned();
            assert!(
                !modules.contains(&format!("md.{basename}")),
                "the repo-root module must not be checkout-named `md.{basename}`: {modules:?}"
            );
            // (2) the heading Structs are unchanged.
            let symbols = export_symbol_fqns(&recs);
            for f in ["md.README.md.root-heading", "md.docs/guide.md.guide"] {
                assert!(
                    symbols.contains(f),
                    "the heading Struct `{f}` must be unchanged: {symbols:?}"
                );
            }
            // (3) root-File containment: the repo-root module Contains the
            // repo-root File (no regression vs the retired `md.apg` root).
            let contains: BTreeSet<(String, String)> = recs
                .iter()
                .filter(|r| r.get("type").and_then(|t| t.as_str()) == Some("contains"))
                .map(|r| {
                    (
                        r.get("from")
                            .and_then(|f| f.as_str())
                            .unwrap_or("")
                            .to_string(),
                        r.get("to")
                            .and_then(|t| t.as_str())
                            .unwrap_or("")
                            .to_string(),
                    )
                })
                .collect();
            assert!(
                contains.contains(&("md.".to_string(), "README.md".to_string())),
                "`md.` must Contain the repo-root `README.md`: {contains:?}"
            );
            assert!(
                contains.contains(&("md.docs".to_string(), "docs/guide.md".to_string())),
                "`md.docs` must Contain `docs/guide.md`: {contains:?}"
            );

            let _ = std::fs::remove_dir_all(&base);
        }

        // -------------------------------------------------------------------
        // libbin-fix (e2e): Rust multi-target crate identity. Scratch Cargo
        // packages under std::env::temp_dir(), driving the staged rust frontend
        // + the in-process ingestor via `rust_frontend_records` — no committed
        // `testdata/**` fixture (testdata is inside apg's own scan scope, so a
        // committed default lib+bin fixture would panic the repo's own scan).
        // -------------------------------------------------------------------

        /// libbin-fix (e2e): a scratch DEFAULT lib + bin Cargo package —
        /// `[package] name = "foo"` with an auto-discovered `src/lib.rs` (lib)
        /// and `src/main.rs` (bin), both rendering the same display name — must
        /// ingest without the duplicate-FQN panic, and its two crate roots must
        /// render DISTINCT module FQNs, each enumerable under its own prefix
        /// (the lib marker under one root, the bin marker under the other).
        ///
        /// The post-fix disambiguating suffix is deliberately NOT pinned: the
        /// frontend owns that spelling. Only the PRESERVATION of already-scanned
        /// FQNs is a contract — pinned by
        /// [`rust_multitarget_crate_fqns_are_stable`].
        #[test]
        #[ignore = "e2e tier: real I/O (scratch Cargo package/spawned rust frontend); run via cargo test-e2e"]
        fn rust_default_libbin_crate_roots_are_distinct() {
            let repo = crate::testutil::Repo::new("rust-libbin-collision");
            repo.write(
                "Cargo.toml",
                "[package]\nname = \"foo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            );
            repo.write("src/lib.rs", "pub struct LibMarker;\n");
            repo.write("src/main.rs", "pub struct BinMarker;\nfn main() {}\n");
            repo.commit_all("fixture");

            let records = rust_frontend_records(&repo.root);
            assert!(
                !records.is_empty(),
                "the rust frontend must emit scanner records for the scratch package"
            );

            // The in-process ingestor is where `insert_node` panics on a
            // duplicate same-kind FQN; completing it without a panic IS the
            // collision assertion (the pre-fix crate roots both rendered
            // `rust.foo`).
            let opts = crate::ingest::IngestOptions {
                blacklist: &[],
                language: "rust",
                config: None,
                base: None,
            };
            let ingested = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                crate::ingest::ingest(records, &opts)
            }))
            .unwrap_or_else(|_| {
                panic!(
                    "ingesting a default lib+bin package must not panic on a \
                     duplicate crate-root FQN (libbin-fix)"
                )
            });
            let (graph, _report) = ingested;

            // The two crate roots are exactly the module nodes parenting each
            // target's marker; they must be DISTINCT, each enumerable with its
            // own subtree.
            let modules: BTreeSet<String> = graph
                .nodes
                .iter()
                .filter(|(_, n)| n.kind == crate::graph::NodeKind::Module)
                .map(|(fqn, _)| fqn.clone())
                .collect();
            let lib_root = modules
                .iter()
                .find(|m| graph.nodes.contains_key(&format!("{m}.LibMarker")))
                .unwrap_or_else(|| panic!("the lib crate root must parent LibMarker: {modules:?}"));
            let bin_root = modules
                .iter()
                .find(|m| graph.nodes.contains_key(&format!("{m}.BinMarker")))
                .unwrap_or_else(|| panic!("the bin crate root must parent BinMarker: {modules:?}"));
            assert!(
                modules.len() >= 2,
                "a default lib+bin package must render two crate-root modules: {modules:?}"
            );
            assert_ne!(
                lib_root, bin_root,
                "the two crate roots must render distinct module FQNs: {modules:?}"
            );

            let _ = std::fs::remove_dir_all(&repo.root);
        }

        /// libbin-fix (e2e) regression: a DISTINCT-name multi-target Cargo
        /// package — `[package] name = "foo"`, `[lib] name = "foo"`,
        /// `[[bin]] name = "foo-cli"` — must keep EXACTLY its current crate-root
        /// module FQNs. The collision-only disambiguation must not leak into a
        /// package whose targets are already distinct, so `rust.foo` and
        /// `rust.foo_cli` are unchanged (rust-analyzer normalizes the bin's
        /// display name to a valid identifier — the literal is `rust.foo_cli`,
        /// underscore).
        #[test]
        #[ignore = "e2e tier: real I/O (scratch Cargo package/spawned rust frontend); run via cargo test-e2e"]
        fn rust_multitarget_crate_fqns_are_stable() {
            let repo = crate::testutil::Repo::new("rust-libbin-stable");
            repo.write(
                "Cargo.toml",
                "[package]\nname = \"foo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
                 [lib]\nname = \"foo\"\n\n\
                 [[bin]]\nname = \"foo-cli\"\npath = \"src/main.rs\"\n",
            );
            repo.write("src/lib.rs", "pub struct LibMarker;\n");
            repo.write("src/main.rs", "pub struct BinMarker;\nfn main() {}\n");
            repo.commit_all("fixture");

            let records = rust_frontend_records(&repo.root);
            let (graph, _report) = crate::ingest::ingest(
                records,
                &crate::ingest::IngestOptions {
                    blacklist: &[],
                    language: "rust",
                    config: None,
                    base: None,
                },
            );

            let modules: BTreeSet<String> = graph
                .nodes
                .iter()
                .filter(|(_, n)| n.kind == crate::graph::NodeKind::Module)
                .map(|(fqn, _)| fqn.clone())
                .collect();
            for pinned in ["rust.foo", "rust.foo_cli"] {
                assert!(
                    modules.contains(pinned),
                    "the distinct-name multi-target package must keep `{pinned}` \
                     unchanged: {modules:?}"
                );
            }
            // …and each marker hangs under its own pinned crate root, so the
            // pinned FQNs are the roots' identities, not incidental.
            assert!(
                graph.nodes.contains_key("rust.foo.LibMarker"),
                "LibMarker must hang under `rust.foo`: {:?}",
                graph.nodes.keys().collect::<Vec<_>>()
            );
            assert!(
                graph.nodes.contains_key("rust.foo_cli.BinMarker"),
                "BinMarker must hang under `rust.foo_cli`: {:?}",
                graph.nodes.keys().collect::<Vec<_>>()
            );

            let _ = std::fs::remove_dir_all(&repo.root);
        }
    }
}
