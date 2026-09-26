//! Scanner frontend discovery, selection, target-set hand-off, and spawning.
//!
//! Extracted from the crate root as a cohesive single-responsibility module
//! (phase-04 decomposition); no behaviour change — the same detection, the same
//! command lines, the same spawn argv, and the same emitted graph.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::classify;
use crate::incremental;
use crate::logging::{Log, tail_of};

/// Directory holding the built scanner frontends. Resolution order:
/// 1. `APG_FRONTEND_DIR` env override.
/// 2. Relative to the running executable: `<exe_dir>/frontends` (dev,
///    `target/<profile>/frontends`) or `<exe_dir>/../libexec/frontends`
///    (brew Cellar layout).
/// 3. `None` — fall back to the compile-time baked paths.
pub fn frontend_dir() -> Option<PathBuf> {
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
pub const STRUCTURAL_LANGUAGES: &[&str] = &[
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

pub fn available_languages() -> Vec<String> {
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
pub fn frontend_cmd(language: &str) -> Option<String> {
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
pub fn has_extension(dir: &std::path::Path, exts: &[&str], depth: u32, skip_set: &[&str]) -> bool {
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
pub fn id_prefix_for(language: &str) -> &'static str {
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
pub fn language_bucket(scan_language: &str) -> &str {
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
pub fn should_spawn_language(
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
pub fn scan_time_tools(language: &str) -> &'static [&'static str] {
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
pub fn scan_time_tool_error(language: &str, tool: &str) -> String {
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
pub fn scan_time_preflight(language: &str) -> anyhow::Result<()> {
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
pub fn spawn_frontend(
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
