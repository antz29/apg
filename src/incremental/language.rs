use std::path::Path;

/// The C++ source/header extensions the C++ frontend recognizes — the dotless
/// mirror of `cpplib.is_cpp_ext` (`src/cpplib/main.cpp`). This is the root
/// crate's single source of truth for C++ membership: [`language_of`]
/// classifies by it and `main::auto_detect_languages` derives its C++ candidate
/// list from it. Keeping one list means a changed `.hxx`/`.tpp`/`.ipp`/`.c++`
/// lands in the C++ target set instead of falling through to `other`, which
/// would leave it in the impact set but in no per-language list — re-emitted by
/// neither the frontend nor the cache, silently dropping its facts.
pub const CPP_EXTENSIONS: &[&str] = &[
    "cpp", "cc", "cxx", "c++", "h", "hpp", "hh", "hxx", "tpp", "ipp",
];

/// The language a checkout-relative path belongs to, by extension — the
/// per-language granularity key for the fact store.
///
/// The returned token is the **scan language id** (`go`, `java`, `rust`, `ts`,
/// `csharp`, `py`, `cpp`, `md`, and the bundled structural scanner's `sh`,
/// `yaml`, `json`, `toml`, `xml`, `dockerfile`, `makefile`, `ini`, `misc`) so
/// it can be compared directly against the scan's detected/requested language
/// ids — `apg.targets_for_language` filters the win-B target set with exactly
/// this comparison. `.py`/`.pyi` therefore return `"py"`, NOT `"python"` (the
/// scan-side vocabulary is `py`: the detector candidate, `frontend_cmd`,
/// `id_prefix_for` and the `lang_switch` record all use it). The same token is
/// the per-file/per-module label in the fact store and module scaffolding
/// (`ReuseFile.lang`, `FileFragment`), where it is only ever compared against
/// other `language_of`-derived labels, so the token rename is internally
/// consistent.
///
/// The structural mappings mirror the bundled scanner's `stream_for_path`
/// taxonomy exactly (its filename-keyed formats included), so each structural
/// stream gets its own changed-file target set and `should_spawn_language` no
/// longer skips a changed structural stream (changed structural facts were
/// silently dropped before). The residual — an unknown extension, a dotfile,
/// a fixture or a binary — is the `misc` stream, matching the scanner's
/// residual, so every tracked file is graphed.
pub fn language_of(rel: &str) -> &'static str {
    let path = Path::new(rel);
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    // Filename-keyed structural formats first: Dockerfile/Makefile and the
    // extension-less config dotfiles carry no (or an ambiguous) extension.
    // Mirrors `structfrontend`'s `stream_for_path`.
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if name == "cargo.lock" {
        return "toml";
    }
    if name == "package-lock.json" {
        return "json";
    }
    if name == "dockerfile" || name.starts_with("dockerfile.") || name.ends_with(".dockerfile") {
        return "dockerfile";
    }
    if name == "makefile" || name == "gnumakefile" || name.ends_with(".mk") {
        return "makefile";
    }
    if name == ".editorconfig"
        || name == ".gitconfig"
        || name == ".env"
        || name.starts_with(".env.")
    {
        return "ini";
    }
    match ext {
        "go" => "go",
        "java" => "java",
        "rs" => "rust",
        "ts" | "tsx" | "mts" | "cts" | "js" | "jsx" | "mjs" | "cjs" => "ts",
        "cs" | "csx" => "csharp",
        "py" | "pyi" => "py",
        _ if CPP_EXTENSIONS.contains(&ext) => "cpp",
        "md" | "markdown" => "md",
        "sh" | "bash" | "zsh" | "ksh" => "sh",
        "yaml" | "yml" => "yaml",
        "json" => "json",
        "toml" => "toml",
        "xml" => "xml",
        "ini" | "cfg" | "conf" | "properties" | "env" => "ini",
        _ => "misc",
    }
}
