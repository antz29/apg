//! `apg/config.json` structural-scope loading and glob matching.

use std::path::Path;

/// The `apg/config.json` structural scope section: include/exclude globs
/// deciding which files the structural scanner claims, plus the structural
/// `code_type`. It is orthogonal to the existing `types` classification rules
/// (scope decides which files; `types` decides their `code_type`) and defaults
/// to ON — an empty include/exclude claims everything the taxonomy routes.
#[derive(serde::Deserialize, Default)]
pub(crate) struct StructuralScope {
    #[serde(default)]
    pub(crate) include: Vec<String>,
    #[serde(default)]
    pub(crate) exclude: Vec<String>,
    #[serde(default)]
    pub(crate) code_type: Option<String>,
}

/// The subset of `apg/config.json` this frontend reads (unknown keys, including
/// the whole `types` list, are ignored).
#[derive(serde::Deserialize, Default)]
struct ConfigFile {
    #[serde(default)]
    structural: Option<StructuralScope>,
}

/// Loads the structural scope section from `apg/config.json` at the repository
/// base (falling back to the legacy `apg.json`), defaulting to ON when absent.
pub(crate) fn load_structural_scope(base: &Path) -> StructuralScope {
    for cand in [base.join("apg/config.json"), base.join("apg.json")] {
        let Ok(text) = std::fs::read_to_string(&cand) else {
            continue;
        };
        if let Ok(cfg) = serde_json::from_str::<ConfigFile>(&text) {
            return cfg.structural.unwrap_or_default();
        }
    }
    StructuralScope::default()
}

/// Simple glob matcher: `*` matches any run (including `/`), `?` matches a
/// single character.
pub(crate) fn glob_match(pattern: &str, path: &str) -> bool {
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
