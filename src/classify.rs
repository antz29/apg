use std::path::Path;

/// A user-defined code type rule: match a path by glob, or a node by name
/// (simple name or full FQN).
#[derive(serde::Deserialize, Default)]
pub struct CodeTypeRule {
    pub name: String,
    #[serde(default)]
    pub globs: Vec<String>,
    #[serde(default)]
    pub names: Vec<String>,
}

/// The `apg/config.json` **structural scope** section: the include/exclude
/// globs deciding which files the bundled structural scanner
/// (`structfrontend`) claims, plus the structural `code_type` it assigns them.
/// It is orthogonal to the existing [`CodeTypeRule`] `types` list — scope
/// decides *which files* are claimed, `types` decides a code record's
/// classification — and mirrors the shape the scanner itself loads
/// (`structfrontend`'s `StructuralScope`: `include`/`exclude`/`code_type`). An
/// absent section is the default ON: an empty include/exclude claims
/// everything the extension taxonomy routes.
#[derive(serde::Deserialize, Default)]
pub struct StructuralScope {
    #[serde(default)]
    pub include: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
    #[serde(default)]
    pub code_type: Option<String>,
}

/// Project-level classification config (apg.json), replacing the built-in
/// defaults when present.
#[derive(serde::Deserialize, Default)]
pub struct ApgConfig {
    #[serde(default = "default_code_type")]
    pub default: String,
    #[serde(default)]
    pub types: Vec<CodeTypeRule>,
    /// The structural-scope section (see [`StructuralScope`]); `None` means the
    /// scanner's defaults (claim everything, `config`).
    #[serde(default)]
    pub structural: Option<StructuralScope>,
}

fn default_code_type() -> String {
    "src".to_string()
}

impl ApgConfig {
    /// Loads the classification config from `apg/config.json` (the committed
    /// layout root), falling back to a legacy `apg.json` at the project root.
    pub fn load(project_dir: &Path) -> Option<ApgConfig> {
        for cand in [
            project_dir.join("apg/config.json"),
            project_dir.join("apg.json"),
        ] {
            let text = std::fs::read_to_string(&cand);
            let Ok(text) = text else {
                continue;
            };
            if let Ok(cfg) = serde_json::from_str(&text) {
                return Some(cfg);
            }
        }
        None
    }
}

/// The bundled structural scanner's stream ids (`structfrontend`'s injected
/// `lang_switch` ids): Markdown plus the per-format text/config/data streams and
/// the residual `misc`. A record on one of these streams takes its `code_type`
/// from the config's [`StructuralScope`] rather than the code `types`/`default`
/// rules; `md` keeps the built-in `docs` classification.
pub fn is_structural_language(language: &str) -> bool {
    matches!(
        language,
        "md" | "sh" | "yaml" | "json" | "toml" | "xml" | "dockerfile" | "makefile" | "ini" | "misc"
    )
}

/// Classifies a Struct/Function node's code type. When a config is present it
/// fully replaces the built-in defaults: the first rule whose glob matches the
/// path or whose name pattern matches the node name/FQN wins, else `default`.
///
/// A STRUCTURAL record is the one exception: instead of falling through to
/// `default` it takes the config's structural `code_type` (default `config`),
/// with `md` keeping its built-in `docs` classification. The `types` rules
/// themselves are unchanged, so a code record's classification is unaffected.
pub fn classify_code_type(
    path: &str,
    fqn: &str,
    language: &str,
    config: Option<&ApgConfig>,
) -> String {
    if let Some(cfg) = config {
        for rule in &cfg.types {
            if rule.globs.iter().any(|g| matches_glob(g, path)) {
                return rule.name.clone();
            }
            if !rule.names.is_empty() {
                let simple = fqn.rsplit('.').next().unwrap_or(fqn);
                if rule
                    .names
                    .iter()
                    .any(|n| matches_glob(n, simple) || matches_glob(n, fqn))
                {
                    return rule.name.clone();
                }
            }
        }
        if is_structural_language(language) {
            return structural_code_type(path, language, cfg);
        }
        return cfg.default.clone();
    }
    builtin_code_type(path, language).to_string()
}

/// The structural `code_type` for a record on `language`: the config's
/// `structural.code_type` when set, else `config` — except Markdown, which
/// keeps the built-in `docs` classification (its generated/external segments
/// included), matching the scanner-side decision that Markdown is documentation
/// while the other structural formats are configuration.
fn structural_code_type(path: &str, language: &str, cfg: &ApgConfig) -> String {
    if language == "md" {
        return builtin_code_type(path, "md").to_string();
    }
    cfg.structural
        .as_ref()
        .and_then(|scope| scope.code_type.clone())
        .unwrap_or_else(|| "config".to_string())
}

/// True when a JavaScript file's FILENAME carries a genuine bundle marker:
/// `*.min.js`, `*.bundle.js`, `*.min.mjs`, `*.bundle.mjs`, `*.min.cjs`,
/// `*.bundle.cjs` (and the `.jsx` analogues). This is extension-keyed, not
/// stream-id-keyed, so it fires under both the `ts` arm (a mixed/TS-detected
/// repo scans its JS under `ts`) and the `js` arm. A bare `.cjs` suffix is NEVER
/// a marker — only the explicit `min`/`bundle` infix.
fn js_bundle_filename(filename_lower: &str) -> bool {
    [".js", ".jsx", ".mjs", ".cjs"].iter().any(|ext| {
        filename_lower
            .strip_suffix(ext)
            .is_some_and(|stem| stem.ends_with(".min") || stem.ends_with(".bundle"))
    })
}

fn builtin_code_type(path: &str, language: &str) -> &'static str {
    let segments: Vec<&str> = path.split('/').collect();
    let has_seg = |names: &[&str]| segments.iter().any(|s| names.contains(s));
    let filename = segments.last().copied().unwrap_or("");
    let filename_lower = filename.to_ascii_lowercase();

    match language {
        "go" => {
            if filename_lower.ends_with("_test.go") || has_seg(&["test", "tests"]) {
                return "test";
            }
            if filename_lower.ends_with(".pb.go") || has_seg(&["gen", "generated"]) {
                return "generated";
            }
            if has_seg(&["vendor"]) {
                return "external";
            }
            "src"
        }
        "java" => {
            if filename_lower.ends_with("test.java")
                || filename_lower.ends_with("tests.java")
                || has_seg(&["test", "tests"])
            {
                return "test";
            }
            if has_seg(&["gen", "generated"]) {
                return "generated";
            }
            if has_seg(&["vendor", "third_party", "thirdparty"]) {
                return "external";
            }
            "src"
        }
        "cpp" => {
            if filename_lower.ends_with("_test.cpp")
                || filename_lower.ends_with("_test.cc")
                || filename_lower.ends_with("_test.c")
                || filename_lower.starts_with("test_")
                || has_seg(&["test", "tests"])
            {
                return "test";
            }
            if filename_lower.ends_with(".pb.cc")
                || filename_lower.ends_with(".pb.h")
                || has_seg(&["gen", "generated"])
            {
                return "generated";
            }
            if has_seg(&["vendor", "third_party", "thirdparty", "external"]) {
                return "external";
            }
            "src"
        }
        "rust" => {
            if filename_lower.ends_with("_test.rs") || has_seg(&["test", "tests"]) {
                return "test";
            }
            if has_seg(&["gen", "generated"]) {
                return "generated";
            }
            if has_seg(&["vendor"]) {
                return "external";
            }
            "src"
        }
        "ts" => {
            if filename_lower.ends_with("_test.ts")
                || filename_lower.ends_with("_test.tsx")
                || filename_lower.ends_with(".test.ts")
                || filename_lower.ends_with(".test.tsx")
                || filename_lower.ends_with(".spec.ts")
                || filename_lower.ends_with(".spec.tsx")
                || has_seg(&["test", "tests", "__tests__"])
            {
                return "test";
            }
            // Extension-keyed JS bundle rule (fires here for a TS-detected repo's
            // incidental JavaScript too: `src/app.min.js` under the `ts` id).
            if js_bundle_filename(&filename_lower)
                || has_seg(&["gen", "generated", "dist", "build", "out"])
            {
                return "generated";
            }
            if has_seg(&["vendor"]) {
                return "external";
            }
            "src"
        }
        "js" => {
            if filename_lower.ends_with("_test.js")
                || filename_lower.ends_with("_test.jsx")
                || filename_lower.ends_with("_test.mjs")
                || filename_lower.ends_with("_test.cjs")
                || filename_lower.ends_with(".test.js")
                || filename_lower.ends_with(".test.jsx")
                || filename_lower.ends_with(".test.mjs")
                || filename_lower.ends_with(".test.cjs")
                || filename_lower.ends_with(".spec.js")
                || filename_lower.ends_with(".spec.jsx")
                || filename_lower.ends_with(".spec.mjs")
                || filename_lower.ends_with(".spec.cjs")
                || has_seg(&["test", "tests", "__tests__"])
            {
                return "test";
            }
            if js_bundle_filename(&filename_lower)
                || has_seg(&["gen", "generated", "dist", "build", "out"])
            {
                return "generated";
            }
            if has_seg(&["vendor"]) {
                return "external";
            }
            "src"
        }
        "csharp" => {
            if filename_lower.ends_with("test.cs")
                || filename_lower.ends_with("tests.cs")
                || filename_lower.starts_with("test")
                || has_seg(&["test", "tests", "Test", "Tests"])
            {
                return "test";
            }
            if filename_lower.ends_with(".g.cs")
                || filename_lower.ends_with(".designer.cs")
                || filename_lower.ends_with(".generated.cs")
                || has_seg(&["gen", "generated", "obj"])
            {
                return "generated";
            }
            if has_seg(&["vendor", "packages", "bin", "third_party", "thirdparty"]) {
                return "external";
            }
            "src"
        }
        // Python: a `.pyi` stub is hand-written (`src`, the `.d.ts` analog) and
        // is never itself a generated marker, while a stub under a
        // `gen`/`generated` tree still classifies generated. `__pycache__` is a
        // generated segment; there is no `site-packages` segment because the
        // discovery exclusion never scans one.
        "py" => {
            if filename_lower.ends_with("_test.py")
                || filename_lower.starts_with("test_")
                || filename_lower.ends_with(".test.py")
                || has_seg(&["test", "tests", "__tests__"])
            {
                return "test";
            }
            if has_seg(&["__pycache__", "gen", "generated"]) {
                return "generated";
            }
            if has_seg(&["vendor", "third_party", "thirdparty"]) {
                return "external";
            }
            "src"
        }
        // Markdown: ordinary docs stay in the graph classified `docs` (all-code-
        // included, filter not omission); generated trees (`gen`/`generated`/
        // `dist`/`build`/`out`) are `generated` and `vendor`/`third_party` are
        // `external`.
        "md" => {
            if has_seg(&["gen", "generated", "dist", "build", "out"]) {
                return "generated";
            }
            if has_seg(&["vendor", "third_party"]) {
                return "external";
            }
            "docs"
        }
        _ => "src",
    }
}

/// The default build-output directory segments the scanner never treats as
/// source: the git store (`.git/**`) and cargo/build output trees
/// (`target/**`). These are the shared path predicate's two default exclusion
/// globs — nothing under them is authored content.
const BUILD_OUTPUT_DIRS: &[&str] = &[".git", "target"];

/// The shared build-output path predicate (the default exclusion globs
/// `.git/**` and `target/**`): true when `path` lies under a default
/// build-output tree — i.e. one of its `/`- or `\`-separated components is
/// `.git` or `target`. `path` may be a scanner path or a repo-relative
/// identity; matching is segment-keyed, so `.gitignore`, `targets/` and
/// `target_file.go` stay ordinary source. Both the scan-hygiene defaults and
/// the ingestor's record drop consult this one predicate, so a tree excluded
/// at the frontend and a tree dropped at ingest can never disagree.
pub fn is_build_output_path(path: &str) -> bool {
    path.split(['/', '\\'])
        .any(|seg| BUILD_OUTPUT_DIRS.contains(&seg))
}

/// Simple glob matcher: `*` matches any run (including `/`), `?` matches a
/// single character.
pub fn matches_glob(pattern: &str, path: &str) -> bool {
    let pat: Vec<char> = pattern.chars().collect();
    let txt: Vec<char> = path.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let mut star = None;
    let mut star_ti = 0usize;
    while ti < txt.len() {
        if pi < pat.len() && (pat[pi] == '?' || pat[pi] == txt[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < pat.len() && pat[pi] == '*' {
            star = Some(pi);
            star_ti = ti;
            pi += 1;
            while pi < pat.len() && pat[pi] == '*' {
                pi += 1;
            }
        } else if let Some(sp) = star {
            pi = sp + 1;
            star_ti += 1;
            ti = star_ti;
        } else {
            return false;
        }
    }
    while pi < pat.len() && pat[pi] == '*' {
        pi += 1;
    }
    pi == pat.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// unit tier -- pure in-memory: no filesystem, database, git or process.
    mod unit {
        use super::*;

        /// fix-module-identity phase-03 task-6: the shared build-output
        /// exclusion predicate rejects every `.git/**` and `target/**` path
        /// (the observed self-ingestion offenders: the Go build cache under
        /// `.git/apg/facts` and the TS/staged frontends under `target/`) and
        /// accepts ordinary source — `.gitignore`, `targets/` and a
        /// `target_file` name are NOT build-output trees.
        #[test]
        fn build_output_predicate_rejects_git_and_target_trees() {
            for p in [
                ".git/apg/facts/go/key/53/abc-d",
                "/abs/repo/.git/index",
                "target/debug/frontends/tsfrontend/scanner.mjs",
                "/abs/repo/target/debug/x.rs",
                "nested/target/out.rs",
                "src\\.git\\x.go",
            ] {
                assert!(is_build_output_path(p), "{p} must be a build-output path");
            }
            for p in [
                "src/main.rs",
                "a/a.go",
                "targets/release.rs",
                "src/target_file.go",
                ".gitignore",
                "docs/.gitkeep",
                "",
            ] {
                assert!(!is_build_output_path(p), "{p} must stay authored source");
            }
        }

        /// apg-0.17.0 phase-01 task-18: with a config present, a STRUCTURAL
        /// record takes the config's structural `code_type` (default `config`)
        /// instead of falling through to `default`; `md` keeps its built-in
        /// `docs` classification; a configured `code_type` overrides the non-md
        /// default; and a code record is unchanged.
        #[test]
        fn structural_streams_take_the_configured_code_type() {
            let default_cfg = ApgConfig {
                default: "src".to_string(),
                types: Vec::new(),
                structural: Some(StructuralScope::default()),
            };
            for lang in [
                "sh",
                "yaml",
                "json",
                "toml",
                "xml",
                "dockerfile",
                "makefile",
                "ini",
                "misc",
            ] {
                assert_eq!(
                    classify_code_type("a/file", "a/file", lang, Some(&default_cfg)),
                    "config",
                    "{lang} must default to `config`"
                );
            }
            assert_eq!(
                classify_code_type("AGENTS.md", "md.AGENTS.md", "md", Some(&default_cfg)),
                "docs",
                "Markdown keeps its built-in docs classification"
            );

            // A configured structural code_type overrides the non-md default,
            // but Markdown still keeps `docs`.
            let custom = ApgConfig {
                default: "src".to_string(),
                types: Vec::new(),
                structural: Some(StructuralScope {
                    include: Vec::new(),
                    exclude: Vec::new(),
                    code_type: Some("cfg".to_string()),
                }),
            };
            assert_eq!(
                classify_code_type("ci.yml", "yaml.ci.yml", "yaml", Some(&custom)),
                "cfg"
            );
            assert_eq!(
                classify_code_type("AGENTS.md", "md.AGENTS.md", "md", Some(&custom)),
                "docs"
            );

            // A code stream is unchanged: it still falls through to `default`,
            // and with no config the built-in classifier is consulted.
            assert_eq!(
                classify_code_type("src/a.rs", "rust.a", "rust", Some(&custom)),
                "src"
            );
            assert_eq!(
                classify_code_type("src/a.rs", "rust.a", "rust", None),
                "src"
            );
        }
    }
}
