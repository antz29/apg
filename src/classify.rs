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

/// Project-level classification config (apg.json), replacing the built-in
/// defaults when present.
#[derive(serde::Deserialize, Default)]
pub struct ApgConfig {
    #[serde(default = "default_code_type")]
    pub default: String,
    #[serde(default)]
    pub types: Vec<CodeTypeRule>,
}

fn default_code_type() -> String {
    "src".to_string()
}

impl ApgConfig {
    pub fn load(project_dir: &Path) -> Option<ApgConfig> {
        let text = std::fs::read_to_string(project_dir.join("apg.json")).ok()?;
        serde_json::from_str(&text).ok()
    }
}

/// Classifies a Struct/Function node's code type. When a config is present it
/// fully replaces the built-in defaults: the first rule whose glob matches the
/// path or whose name pattern matches the node name/FQN wins, else `default`.
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
        return cfg.default.clone();
    }
    builtin_code_type(path, language).to_string()
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
        _ => "src",
    }
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
