//! apg structural frontend (`structfrontend`).
//!
//! One self-contained Rust binary that walks a checkout, claims every tracked
//! file no code frontend claims — shell, YAML, JSON, TOML, XML, Dockerfile,
//! Makefile, the INI family, Markdown, and the residual `misc` — and streams
//! the unified JSONL facts (SPEC §2) the Rust ingestor consumes: one `Module`
//! per directory identity, one `File` per claimed file, one `Struct` per
//! declared structure, plus the `contains` edges that nest them.
//!
//! It absorbs the retired `mdfrontend` Markdown frontend: the `md` stream id is
//! preserved and the heading-Struct mechanism is unchanged. The one deliberate
//! identity change is the emitted **module identity**: it is REPO-RELATIVE to
//! the repository base (empty at the repo root) instead of the absolute parent
//! directory, so the repo-root Markdown module renders `md.` rather than the
//! checkout basename (`md.apg`); every non-root `md.*` module and heading
//! Struct FQN is unchanged.
//!
//! It emits **facts only** — it never computes FQNs and never assembles the
//! graph — and is self-contained: it never shells out and needs nothing on
//! `PATH` at scan time.
//!
//! Stream ids (the scan driver injects one `lang_switch` per spawned stream,
//! and passes the matching `--stream <id>` selector): `md`, `sh`, `yaml`,
//! `json`, `toml`, `xml`, `dockerfile`, `makefile`, `ini`, and the residual
//! `misc`.
//!
//! Usage:
//! `structfrontend <dir> [--stream <id>] [--module <dir>]... [--id-prefix <p>]
//!  [--targets <file>] [--cache-dir <dir>] [--cache-key <key>] [exclude...]`.

use std::collections::{BTreeSet, HashSet};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use unicode_normalization::UnicodeNormalization;

/// Directories never descended into: the git store, cargo/build output, the
/// package manager's tree, and the project-worktree store (a nested checkout
/// of the same repository). Generated trees (`gen`, `generated`, `dist`,
/// `build`, `out`) are deliberately NOT here: every accepted file stays in the
/// graph and is filtered by `code_type`.
///
/// `.git` and `.worktrees` also happen to be hidden, but the taxonomy does not
/// prune hidden trees wholesale — `.github/`, `.cargo/` and the config dotfiles
/// are claimed like any other authored content.
const EXCLUDED_DIRS: &[&str] = &["target", "node_modules", ".git", ".worktrees"];

/// The code-frontend extensions the structural scanner never claims: a file a
/// code frontend parses stays that frontend's. This is the UNION of every
/// shipped code frontend's accepted extensions — Rust/Go/Java/C#/Python
/// (`rs go java cs py pyi`), C++'s sources and headers
/// (`cpp cc cxx c++ h hpp hh hxx tpp ipp`), and the unified JS/TS module
/// variants (`ts tsx mts cts js jsx mjs cjs`). `.c` is deliberately absent —
/// the C++ frontend does not claim it, so a tracked `.c` file falls to the
/// residual `misc` stream.
const CODE_EXTENSIONS: &[&str] = &[
    "rs", "go", "java", "cpp", "cc", "cxx", "c++", "h", "hpp", "hh", "hxx", "tpp", "ipp", "ts",
    "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs", "cs", "py", "pyi",
];

/// Unified JSONL records (SPEC §2). `id` is a scanner-local opaque counter; the
/// ingestor maps ids to canonical FQNs and renders `parent.name`.
#[derive(serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Rec {
    Module {
        fqn: String,
    },
    File {
        path: String,
        parent: String,
        start_line: u32,
        end_line: u32,
    },
    Struct {
        id: String,
        parent: String,
        name: String,
        path: String,
        start: u32,
        end: u32,
        start_line: u32,
        end_line: u32,
    },
    Contains {
        from: String,
        to: String,
    },
}

/// Parsed command line (the pinned frontend hand-off plus `--module` /
/// `--id-prefix` / the per-stream selector). The cache flags are consumed and
/// dropped — there is no native artifact to key.
struct Args {
    root: PathBuf,
    /// Per-stream selector: `Some(id)` emits only that stream's records (how
    /// the scan driver spawns one stream at a time); `None` runs the full
    /// structural walk and emits the union of every stream's records.
    stream: Option<String>,
    module_dirs: Vec<String>,
    id_prefix: String,
    targets_path: Option<String>,
    excludes: Vec<String>,
}

fn usage() -> String {
    "usage: structfrontend <dir> [--stream <id>] [--module <dir>]... [--id-prefix <p>] \
     [--targets <file>] [--cache-dir <dir>] [--cache-key <key>] [exclude...]"
        .to_string()
}

fn parse_args(argv: &[String]) -> Result<Args, String> {
    let Some(first) = argv.first() else {
        return Err(usage());
    };
    let mut args = Args {
        root: PathBuf::from(first),
        stream: None,
        module_dirs: Vec::new(),
        id_prefix: "n".to_string(),
        targets_path: None,
        excludes: Vec::new(),
    };
    let mut i = 1;
    while i < argv.len() {
        let next = argv.get(i + 1).cloned();
        match argv[i].as_str() {
            "--stream" => match next {
                Some(v) => {
                    args.stream = Some(v);
                    i += 2;
                }
                None => {
                    args.excludes.push(argv[i].clone());
                    i += 1;
                }
            },
            "--module" => match next {
                Some(v) => {
                    args.module_dirs.push(v);
                    i += 2;
                }
                None => {
                    args.excludes.push(argv[i].clone());
                    i += 1;
                }
            },
            "--id-prefix" => match next {
                Some(v) => {
                    args.id_prefix = v;
                    i += 2;
                }
                None => {
                    args.excludes.push(argv[i].clone());
                    i += 1;
                }
            },
            "--targets" => match next {
                Some(v) => {
                    args.targets_path = Some(v);
                    i += 2;
                }
                None => {
                    args.excludes.push(argv[i].clone());
                    i += 1;
                }
            },
            // Accepted for the pinned hand-off, then ignored (no native cache).
            "--cache-dir" | "--cache-key" => match next {
                Some(_) => i += 2,
                None => i += 1,
            },
            other => {
                args.excludes.push(other.to_string());
                i += 1;
            }
        }
    }
    Ok(args)
}

/// Lexical-ish absolute path: `canonicalize` when it exists (so a symlinked
/// scan root still matches the target list), else the path made absolute.
fn absolutize(p: &Path) -> PathBuf {
    if let Ok(c) = std::fs::canonicalize(p) {
        return c;
    }
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|d| d.join(p))
            .unwrap_or_else(|_| p.to_path_buf())
    }
}

/// Reads the pinned `--targets` list: absolute source paths, one per line,
/// blanks ignored. `None` — an absent flag, an unreadable file or an empty list
/// — means NO emission filter (the byte-identical full scan).
fn read_target_set(path: Option<&str>) -> Option<HashSet<PathBuf>> {
    let path = path?;
    let Ok(text) = std::fs::read_to_string(path) else {
        eprintln!("warning: could not read targets {path}; scanning unfiltered");
        return None;
    };
    let set: HashSet<PathBuf> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| absolutize(Path::new(l)))
        .collect();
    if set.is_empty() {
        None
    } else {
        Some(set)
    }
}

/// The `apg/config.json` structural scope section: include/exclude globs
/// deciding which files the structural scanner claims, plus the structural
/// `code_type`. It is orthogonal to the existing `types` classification rules
/// (scope decides which files; `types` decides their `code_type`) and defaults
/// to ON — an empty include/exclude claims everything the taxonomy routes.
#[derive(serde::Deserialize, Default)]
struct StructuralScope {
    #[serde(default)]
    include: Vec<String>,
    #[serde(default)]
    exclude: Vec<String>,
    #[serde(default)]
    code_type: Option<String>,
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
fn load_structural_scope(base: &Path) -> StructuralScope {
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
fn glob_match(pattern: &str, path: &str) -> bool {
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

/// The repository base: the nearest ancestor of `root` carrying a `.git` entry
/// (a directory in a primary checkout, a file in a linked worktree), else
/// `root` itself (a non-git scan root). Every emitted module identity is
/// rendered relative to this base.
fn repo_base(root: &Path) -> PathBuf {
    let mut cur = root;
    loop {
        if cur.join(".git").exists() {
            return cur.to_path_buf();
        }
        match cur.parent() {
            Some(parent) => cur = parent,
            None => return root.to_path_buf(),
        }
    }
}

/// `path`'s identity relative to `base`: its `/`-joined normal components, with
/// no leading separator. `None` when `path` is not under `base`.
fn relative_join(base: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(base).ok()?;
    let parts: Vec<String> = rel
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    Some(parts.join("/"))
}

/// The emitted module identity of `path`'s directory: repo-relative to `base`,
/// EMPTY at the repository base itself.
fn repo_relative_dir(base: &Path, path: &Path) -> String {
    relative_join(base, path.parent().unwrap_or(path)).unwrap_or_default()
}

/// The emitted identity of `path` itself (file name included), repo-relative to
/// `base`; falls back to the bare file name for a foreign/escaping path so an
/// identity never embeds a checkout component.
fn repo_relative_identity(base: &Path, path: &Path) -> String {
    relative_join(base, path).unwrap_or_else(|| {
        path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    })
}

/// The COMPLETE extension/filename → stream-id taxonomy. `None` when a code
/// frontend claims the file (its extension is in [`CODE_EXTENSIONS`]); `Some`
/// with the residual `misc` id otherwise, so every unclaimed file is claimed by
/// exactly one structural stream.
fn stream_for_path(path: &Path) -> Option<&'static str> {
    let name = path.file_name().and_then(|n| n.to_str())?;
    let lower = name.to_ascii_lowercase();
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());

    if let Some(e) = ext.as_deref() {
        if CODE_EXTENSIONS.contains(&e) {
            return None;
        }
    }

    // Filename-keyed formats first: Dockerfile/Makefile and the extension-less
    // config dotfiles carry no (or an ambiguous) extension.
    if lower == "dockerfile" || lower.starts_with("dockerfile.") || lower.ends_with(".dockerfile") {
        return Some("dockerfile");
    }
    if lower == "makefile" || lower == "gnumakefile" || lower.ends_with(".mk") {
        return Some("makefile");
    }
    if lower == "cargo.lock" {
        return Some("toml");
    }
    if lower == "package-lock.json" {
        return Some("json");
    }
    if lower == ".editorconfig"
        || lower == ".gitconfig"
        || lower == ".env"
        || lower.starts_with(".env.")
    {
        return Some("ini");
    }

    Some(match ext.as_deref() {
        Some("md") | Some("markdown") => "md",
        Some("sh") | Some("bash") | Some("zsh") | Some("ksh") => "sh",
        Some("yaml") | Some("yml") => "yaml",
        Some("json") => "json",
        Some("toml") => "toml",
        Some("xml") => "xml",
        Some("ini") | Some("cfg") | Some("conf") | Some("properties") | Some("env") => "ini",
        // Everything the taxonomy does not name — an unknown extension, a
        // dotfile (`.gitignore`, `.dockerignore`), a fixture or a binary — is
        // the residual `misc` stream.
        _ => "misc",
    })
}

fn is_pruned_dir(name: &str) -> bool {
    EXCLUDED_DIRS.contains(&name)
}

/// True when `path` carries a discovery-excluded directory component BELOW
/// `root` (so a scan root that itself lives under `.worktrees/` is still
/// scanned, while a `target/`, `node_modules/` or nested `.worktrees/` tree
/// inside it is not).
fn under_excluded_tree(path: &Path, root: &Path) -> bool {
    let rel = path.strip_prefix(root).unwrap_or(path);
    rel.components().any(|c| match c {
        std::path::Component::Normal(s) => is_pruned_dir(&s.to_string_lossy()),
        _ => false,
    })
}

/// The claim/scope boundary: pruned trees, `--module` restriction,
/// `--exclude-path` substrings, and the `apg/config.json` structural scope
/// include/exclude globs (matched against the repo-relative identity).
fn in_scope(
    path: &Path,
    root: &Path,
    base: &Path,
    module_dirs: &[PathBuf],
    excludes: &[String],
    scope: &StructuralScope,
) -> bool {
    if under_excluded_tree(path, root) {
        return false;
    }
    if !module_dirs.is_empty() && !module_dirs.iter().any(|m| path.starts_with(m)) {
        return false;
    }
    let s = path.to_string_lossy();
    if excludes.iter().any(|x| s.contains(x.as_str())) {
        return false;
    }
    let rel = repo_relative_identity(base, path);
    if !scope.include.is_empty() && !scope.include.iter().any(|g| glob_match(g, &rel)) {
        return false;
    }
    if scope.exclude.iter().any(|g| glob_match(g, &rel)) {
        return false;
    }
    true
}

/// Every claimed file at or below `dir`, paired with its stream id (the
/// full-scan discovery walk). Code-frontend extensions and the pruned directory
/// trees are skipped here; `--module`/`--exclude-path`/config scope are applied
/// by the caller so the walk stays the pure taxonomy.
fn walk(dir: &Path, out: &mut Vec<(PathBuf, &'static str)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(ft) = entry.file_type() else { continue };
        let path = entry.path();
        if ft.is_dir() {
            if is_pruned_dir(&entry.file_name().to_string_lossy()) {
                continue;
            }
            walk(&path, out);
        } else if ft.is_file() {
            if let Some(stream) = stream_for_path(&path) {
                out.push((path, stream));
            }
        }
    }
}

/// One ATX heading found in a document.
struct RawHeading {
    level: u8,
    start: u32,
    end: u32,
    start_line: u32,
    end_line: u32,
    text: String,
}

/// A section symbol: one `Struct` record plus the nesting needed for its
/// `contains` edge.
struct Section {
    level: u8,
    id: String,
    /// The parent FQN for the record (the File path, or the parent section's).
    parent: String,
    /// `parent.name` — the section's rendered FQN (used to parent its children).
    fqn: String,
    name: String,
    start: u32,
    end: u32,
    start_line: u32,
    end_line: u32,
    parent_index: Option<usize>,
}

/// A document's emitted facts.
struct Doc {
    path: String,
    /// The repo-relative module identity of the document's directory (empty at
    /// the repository base).
    dir: String,
    sections: Vec<Section>,
}

/// A heading whose trailing closing `#`s are stripped (CommonMark: a trailing
/// run of `#` preceded by whitespace is an optional closing sequence).
fn strip_closing_hashes(s: &str) -> &str {
    let t = s.trim_end();
    let run = t.bytes().rev().take_while(|&b| b == b'#').count();
    if run == 0 {
        return t;
    }
    let before = &t[..t.len() - run];
    if before.is_empty() || before.ends_with(' ') || before.ends_with('\t') {
        before.trim_end()
    } else {
        t
    }
}

/// ATX headings (`#`..`######`, up to 3 leading spaces, space-or-EOL after the
/// run), skipping fenced code blocks so a `#` comment in a fence is not a
/// section. Byte ranges exclude the line terminator; line numbers are 1-based.
fn parse_headings(text: &str) -> Vec<RawHeading> {
    let mut out = Vec::new();
    let mut offset: u32 = 0;
    let mut line_no: u32 = 1;
    let mut fence: Option<(u8, usize)> = None;
    for raw in text.split_inclusive('\n') {
        let without_nl = raw.strip_suffix('\n').unwrap_or(raw);
        let line = without_nl.strip_suffix('\r').unwrap_or(without_nl);
        let line_start = offset;
        let line_end = offset + line.len() as u32;
        offset += raw.len() as u32;

        let lead = line.bytes().take_while(|&b| b == b' ').count();
        let rest = if lead <= 3 { &line[lead..] } else { "" };

        if lead <= 3 {
            if let Some((fch, flen)) = fence {
                // A closing fence is the same char, >= the opening length, with
                // only whitespace after it.
                let run = rest.bytes().take_while(|&b| b == fch).count();
                if run >= flen && rest[run..].trim().is_empty() {
                    fence = None;
                    line_no += 1;
                    continue;
                }
            } else if let Some(fch) = rest.bytes().next().filter(|b| *b == b'`' || *b == b'~') {
                let run = rest.bytes().take_while(|&b| b == fch).count();
                if run >= 3 {
                    fence = Some((fch, run));
                    line_no += 1;
                    continue;
                }
            }
        }
        if fence.is_some() {
            line_no += 1;
            continue;
        }

        if lead <= 3 {
            let hashes = rest.bytes().take_while(|&b| b == b'#').count();
            if (1..=6).contains(&hashes) {
                let after = &rest[hashes..];
                if after.is_empty() || after.starts_with(' ') || after.starts_with('\t') {
                    out.push(RawHeading {
                        level: hashes as u8,
                        start: line_start,
                        end: line_end,
                        start_line: line_no,
                        end_line: line_no,
                        text: strip_closing_hashes(after.trim()).to_string(),
                    });
                }
            }
        }
        line_no += 1;
    }
    out
}

/// Collapses runs of `-` to one and trims leading/trailing `-`.
fn collapse_and_trim(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_dash = false;
    for ch in s.chars() {
        if ch == '-' {
            if out.is_empty() || prev_dash {
                prev_dash = true;
                continue;
            }
            out.push('-');
            prev_dash = true;
        } else {
            out.push(ch);
            prev_dash = false;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// A heading's RAW bare slug: trim, NFC, lowercase, whitespace runs -> `-`,
/// drop punctuation except `-`/`_`, preserve non-ASCII letters/digits, then
/// collapse + trim `-`. Empty normalizations fall back to the pinned literal
/// `section` (an ordinary bare slug that participates in dedup).
fn bare_slug(heading: &str) -> String {
    let nfc: String = heading.trim().nfc().collect();
    let lower = nfc.to_lowercase();
    let mut out = String::with_capacity(lower.len());
    let mut pending_space = false;
    for ch in lower.chars() {
        if ch.is_whitespace() {
            if !out.is_empty() {
                pending_space = true;
            }
            continue;
        }
        if !(ch.is_alphanumeric() || ch == '-' || ch == '_') {
            continue;
        }
        if pending_space {
            out.push('-');
            pending_space = false;
        }
        out.push(ch);
    }
    let collapsed = collapse_and_trim(&out);
    if collapsed.is_empty() {
        "section".to_string()
    } else {
        collapsed
    }
}

/// Splits a raw bare slug ending in `-<digits>` into (`stem-`, n); `None`
/// otherwise. The candidate sequence evolves the slug's OWN trailing number.
fn split_trailing_number(bare: &str) -> Option<(&str, u64)> {
    let dash = bare.rfind('-')?;
    let digits = &bare[dash + 1..];
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n: u64 = digits.parse().ok()?;
    Some((&bare[..=dash], n))
}

/// The first free candidate in the raw slug's candidate sequence, inserted into
/// `assigned`. Injective within a document by construction.
fn assign_segment(bare: &str, assigned: &mut HashSet<String>) -> String {
    match split_trailing_number(bare) {
        Some((stem, n)) => {
            let mut k = n;
            loop {
                let candidate = format!("{stem}{k}");
                if assigned.insert(candidate.clone()) {
                    return candidate;
                }
                k += 1;
            }
        }
        None => {
            let mut k: u64 = 0;
            loop {
                let candidate = if k == 0 {
                    bare.to_string()
                } else {
                    format!("{bare}-{k}")
                };
                if assigned.insert(candidate.clone()) {
                    return candidate;
                }
                k += 1;
            }
        }
    }
}

/// The PURE seam: heading texts -> assigned FQN segments, in document order,
/// independent of FQN rendering (`requirements.constraint.markdown-slug-rule`).
fn slug_segments(headings: &[&str]) -> Vec<String> {
    let mut assigned: HashSet<String> = HashSet::new();
    headings
        .iter()
        .map(|h| assign_segment(&bare_slug(h), &mut assigned))
        .collect()
}

/// Mirrors the other frontends' file line count: `"a\nb\n"` -> 2, `""` -> 1.
fn line_count(text: &str) -> u32 {
    if text.is_empty() {
        return 1;
    }
    let n = text.bytes().filter(|b| *b == b'\n').count() as u32;
    if text.ends_with('\n') {
        n
    } else {
        n + 1
    }
}

/// Builds one document's facts: slug the headings, nest by heading level (the
/// nearest preceding lower-level heading, else the File), assign opaque ids,
/// and render the module identity repo-relative to `base` (empty at the base).
fn build_doc(path: &Path, bytes: &[u8], base: &Path, id_prefix: &str, next_id: &mut u64) -> Doc {
    let text = String::from_utf8_lossy(bytes);
    let headings = parse_headings(&text);
    let texts: Vec<&str> = headings.iter().map(|h| h.text.as_str()).collect();
    let slugs = slug_segments(&texts);
    let path_str = path.to_string_lossy().into_owned();
    let dir = repo_relative_dir(base, path);

    let mut sections: Vec<Section> = Vec::with_capacity(headings.len());
    let mut stack: Vec<usize> = Vec::new();
    for (h, slug) in headings.into_iter().zip(slugs) {
        while let Some(&top) = stack.last() {
            if sections[top].level >= h.level {
                stack.pop();
            } else {
                break;
            }
        }
        let parent_index = stack.last().copied();
        let parent = match parent_index {
            Some(pi) => sections[pi].fqn.clone(),
            None => path_str.clone(),
        };
        let fqn = format!("{parent}.{slug}");
        let n = *next_id;
        *next_id += 1;
        sections.push(Section {
            level: h.level,
            id: format!("{id_prefix}{n}"),
            parent,
            fqn,
            name: slug,
            start: h.start,
            end: h.end,
            start_line: h.start_line,
            end_line: h.end_line,
            parent_index,
        });
        stack.push(sections.len() - 1);
    }
    Doc {
        path: path_str,
        dir,
        sections,
    }
}

/// The structure facts one format emitter produces for a single claimed file:
/// its `Struct` records plus the `contains` edges that nest them. An emitter
/// NEVER emits `Module`/`File` — `run` assembles those around it — and draws
/// opaque ids from the shared `id_prefix`/`next_id` counter so ids stay unique
/// across the whole spawn.
struct Structure {
    structs: Vec<Rec>,
    contains: Vec<Rec>,
}

fn write_rec<W: Write>(w: &mut W, rec: &Rec) -> io::Result<()> {
    let line = serde_json::to_string(rec).expect("serialize record");
    writeln!(w, "{line}")
}

fn run(args: Args) -> io::Result<()> {
    let root = absolutize(&args.root);
    let base = repo_base(&root);
    let scope = load_structural_scope(&base);
    let module_dirs: Vec<PathBuf> = args
        .module_dirs
        .iter()
        .map(|d| absolutize(&root.join(d)))
        .collect();
    let target_set = read_target_set(args.targets_path.as_deref());
    let selected = args.stream.as_deref();

    // The configured structural `code_type` names the structural classification
    // — Markdown keeps `docs`, every other structural stream defaults to
    // `config`. The scanner emits facts only (the ingestor computes `code_type`
    // from the stream id + the same config section), so this read completes the
    // scope section's consumption without emitting a `code_type` field.
    let _structural_code_type = scope.code_type.as_deref().unwrap_or("config");

    let mut files: Vec<(PathBuf, &'static str)> = match &target_set {
        Some(set) => set
            .iter()
            .filter_map(|p| stream_for_path(p).map(|s| (p.clone(), s)))
            .filter(|(p, _)| p.is_file() && p.starts_with(&root))
            .collect(),
        None => {
            let mut out = Vec::new();
            walk(&root, &mut out);
            out
        }
    };
    files.retain(|(p, _)| in_scope(p, &root, &base, &module_dirs, &args.excludes, &scope));
    // The per-stream selector: a spawn emits only its own stream's records.
    if let Some(sel) = selected {
        files.retain(|(_, s)| *s == sel);
    }
    files.sort();
    files.dedup();

    let stdout = io::stdout();
    let mut writer = io::BufWriter::new(stdout.lock());

    let mut modules: BTreeSet<String> = BTreeSet::new();
    let mut file_recs: Vec<Rec> = Vec::new();
    let mut struct_recs: Vec<Rec> = Vec::new();
    let mut contains_recs: Vec<Rec> = Vec::new();
    let mut next_id: u64 = 1;

    for (path, stream) in &files {
        let Ok(bytes) = std::fs::read(path) else {
            eprintln!("warning: could not read {}", path.display());
            continue;
        };
        let text = String::from_utf8_lossy(&bytes);
        let (module, structure) = match *stream {
            // The absorbed Markdown emitter: `build_doc` slugs and nests the
            // heading sections and yields the repo-relative module identity.
            "md" => {
                let doc = build_doc(path, &bytes, &base, &args.id_prefix, &mut next_id);
                let structure = Structure {
                    structs: doc
                        .sections
                        .iter()
                        .map(|s| Rec::Struct {
                            id: s.id.clone(),
                            parent: s.parent.clone(),
                            name: s.name.clone(),
                            path: doc.path.clone(),
                            start: s.start,
                            end: s.end,
                            start_line: s.start_line,
                            end_line: s.end_line,
                        })
                        .collect(),
                    contains: doc
                        .sections
                        .iter()
                        .filter_map(|s| {
                            s.parent_index.map(|pi| Rec::Contains {
                                from: doc.sections[pi].id.clone(),
                                to: s.id.clone(),
                            })
                        })
                        .collect(),
                };
                (doc.dir, structure)
            }
            // The per-format dispatch table (`emit_sh`..`emit_misc`): each
            // returns this file's `Struct`/`contains` facts, and `run` owns the
            // surrounding `Module`/`File` records.
            "sh" => (
                repo_relative_dir(&base, path),
                emit_sh(path, &bytes, &args.id_prefix, &mut next_id),
            ),
            "yaml" => (
                repo_relative_dir(&base, path),
                emit_yaml(path, &bytes, &args.id_prefix, &mut next_id),
            ),
            "json" => (
                repo_relative_dir(&base, path),
                emit_json(path, &bytes, &args.id_prefix, &mut next_id),
            ),
            "toml" => (
                repo_relative_dir(&base, path),
                emit_toml(path, &bytes, &args.id_prefix, &mut next_id),
            ),
            "xml" => (
                repo_relative_dir(&base, path),
                emit_xml(path, &bytes, &args.id_prefix, &mut next_id),
            ),
            "dockerfile" => (
                repo_relative_dir(&base, path),
                emit_dockerfile(path, &bytes, &args.id_prefix, &mut next_id),
            ),
            "makefile" => (
                repo_relative_dir(&base, path),
                emit_makefile(path, &bytes, &args.id_prefix, &mut next_id),
            ),
            "ini" => (
                repo_relative_dir(&base, path),
                emit_ini(path, &bytes, &args.id_prefix, &mut next_id),
            ),
            // The residual `misc` stream emits a `File` record only.
            _ => (
                repo_relative_dir(&base, path),
                emit_misc(path, &bytes, &args.id_prefix, &mut next_id),
            ),
        };
        modules.insert(module.clone());
        file_recs.push(Rec::File {
            path: path.to_string_lossy().into_owned(),
            parent: module,
            start_line: 1,
            end_line: line_count(&text),
        });
        struct_recs.extend(structure.structs);
        contains_recs.extend(structure.contains);
    }

    for fqn in &modules {
        write_rec(&mut writer, &Rec::Module { fqn: fqn.clone() })?;
    }
    for rec in &file_recs {
        write_rec(&mut writer, rec)?;
    }
    for rec in &struct_recs {
        write_rec(&mut writer, rec)?;
    }
    for rec in &contains_recs {
        write_rec(&mut writer, rec)?;
    }
    writer.flush()
}

// ── Per-format structure emitters ────────────────────────────────────────────
//
// Each emitter returns ONE claimed file's `Struct`/`contains` facts (never
// `Module`/`File` — `run` owns those) under the pinned
// `(path, bytes, id_prefix, next_id)` shape, drawing opaque ids from the shared
// counter. Structure depth is deliberately minimal but useful
// (`requirements.constraint.structural-format-structure-table`): shell
// functions, top-level keys/jobs/steps, tables/keys, elements,
// stages/instructions, targets/variables/includes, sections/keys.

/// One physical source line: its 1-based number and the byte range of its
/// content (line terminator excluded, a trailing `\r` stripped).
struct SourceLine<'a> {
    no: u32,
    start: u32,
    end: u32,
    text: &'a str,
}

/// A byte + 1-based-line range, the location shape every `Struct` record
/// carries.
#[derive(Clone, Copy)]
struct Span {
    start: u32,
    end: u32,
    start_line: u32,
    end_line: u32,
}

impl SourceLine<'_> {
    fn span(&self) -> Span {
        Span {
            start: self.start,
            end: self.end,
            start_line: self.no,
            end_line: self.no,
        }
    }
}

/// The span covering `first..=last` (first line's start through last line's
/// end).
fn range_span(first: &SourceLine<'_>, last: &SourceLine<'_>) -> Span {
    Span {
        start: first.start,
        end: last.end,
        start_line: first.no,
        end_line: last.no,
    }
}

/// Splits `text` into [`SourceLine`]s. Byte ranges exclude the line terminator;
/// line numbers are 1-based, matching `parse_headings`/`line_count`.
fn source_lines(text: &str) -> Vec<SourceLine<'_>> {
    let mut out = Vec::new();
    let mut offset: u32 = 0;
    for (no, raw) in (1u32..).zip(text.split_inclusive('\n')) {
        let without_nl = raw.strip_suffix('\n').unwrap_or(raw);
        let line = without_nl.strip_suffix('\r').unwrap_or(without_nl);
        let start = offset;
        let end = offset + line.len() as u32;
        offset += raw.len() as u32;
        out.push(SourceLine {
            no,
            start,
            end,
            text: line,
        });
    }
    out
}

/// The index of the line containing byte `offset` (the last line whose start
/// precedes it).
fn line_index_at(lines: &[SourceLine<'_>], offset: usize) -> usize {
    let mut idx = 0usize;
    for (i, l) in lines.iter().enumerate() {
        if l.start as usize <= offset {
            idx = i;
        } else {
            break;
        }
    }
    idx
}

/// Strips one matching pair of surrounding single/double quotes.
fn unquote(s: &str) -> String {
    let b = s.as_bytes();
    if b.len() >= 2 && (b[0] == b'"' || b[0] == b'\'') && b[b.len() - 1] == b[0] {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

/// Accumulates one file's structure facts: the `Struct` records plus the
/// parent→child `contains` edges that nest them. A top-level entry is
/// file-rooted (its `parent` is the file path, mirroring the absorbed md
/// heading Structs); a nested entry's parent is its parent's rendered FQN.
/// Names are disambiguated per parent so no two records in a file render the
/// same `parent.name` FQN (the ingestor panics on a duplicate claim).
struct StructureBuilder<'a> {
    path: String,
    id_prefix: &'a str,
    next_id: &'a mut u64,
    structs: Vec<Rec>,
    contains: Vec<Rec>,
    assigned: HashSet<String>,
}

impl<'a> StructureBuilder<'a> {
    fn new(path: &str, id_prefix: &'a str, next_id: &'a mut u64) -> Self {
        Self {
            path: path.to_string(),
            id_prefix,
            next_id,
            structs: Vec::new(),
            contains: Vec::new(),
            assigned: HashSet::new(),
        }
    }

    /// Adds one `Struct` under `parent` — `(id, fqn)` of the parent, or `None`
    /// for a file-rooted top-level entry — returning the new `(id, fqn)` so the
    /// caller can nest children under it. The `contains` edge parent→child is
    /// emitted only for a nested entry.
    fn add(&mut self, parent: Option<(&str, &str)>, name: &str, span: Span) -> (String, String) {
        let (parent_id, parent_fqn) = match parent {
            Some((id, fqn)) => (Some(id), fqn.to_string()),
            None => (None, self.path.clone()),
        };
        let name = self.unique(&parent_fqn, name);
        let fqn = format!("{parent_fqn}.{name}");
        let id = format!("{}{}", self.id_prefix, *self.next_id);
        *self.next_id += 1;
        if let Some(pid) = parent_id {
            self.contains.push(Rec::Contains {
                from: pid.to_string(),
                to: id.clone(),
            });
        }
        self.structs.push(Rec::Struct {
            id: id.clone(),
            parent: parent_fqn,
            name,
            path: self.path.clone(),
            start: span.start,
            end: span.end,
            start_line: span.start_line,
            end_line: span.end_line,
        });
        (id, fqn)
    }

    /// Disambiguates `name` among its siblings so `parent.name` is unique in
    /// this file: the first occurrence keeps `name`, later ones get `-1`, `-2`, …
    fn unique(&mut self, parent: &str, name: &str) -> String {
        let base = if name.is_empty() { "section" } else { name };
        let mut candidate = base.to_string();
        let mut k: u64 = 0;
        loop {
            if self.assigned.insert(format!("{parent}.{candidate}")) {
                return candidate;
            }
            k += 1;
            candidate = format!("{base}-{k}");
        }
    }

    fn finish(self) -> Structure {
        Structure {
            structs: self.structs,
            contains: self.contains,
        }
    }
}

/// The `sh` emitter: shell function definitions (POSIX `name()` / `name ()`
/// and the bash `function name` keyword) → one `Struct` each, spanning the
/// definition line through its closing `}`.
fn emit_sh(path: &Path, bytes: &[u8], id_prefix: &str, next_id: &mut u64) -> Structure {
    let text = String::from_utf8_lossy(bytes);
    let lines = source_lines(&text);
    let mut b = StructureBuilder::new(&path.to_string_lossy(), id_prefix, next_id);
    for (i, line) in lines.iter().enumerate() {
        if let Some(name) = sh_function_name(line.text) {
            let end = sh_block_end(&lines, i);
            b.add(None, &name, range_span(line, &lines[end]));
        }
    }
    b.finish()
}

/// The name of a shell function defined on `line`, or `None`.
fn sh_function_name(line: &str) -> Option<String> {
    let s = line.trim_start();
    if let Some(rest) = s.strip_prefix("function") {
        if rest.starts_with([' ', '\t']) {
            return shell_identifier(rest.trim_start());
        }
    }
    let name = shell_identifier(s)?;
    let after = s[name.len()..].trim_start();
    if after.starts_with('(') && after[1..].trim_start().starts_with(')') {
        return Some(name);
    }
    None
}

/// A POSIX shell name (`[A-Za-z_][A-Za-z0-9_]*`) at the start of `s`.
fn shell_identifier(s: &str) -> Option<String> {
    let mut chars = s.chars();
    let first = chars.next()?;
    if !(first.is_ascii_alphabetic() || first == '_') {
        return None;
    }
    let mut end = first.len_utf8();
    for c in chars {
        if c.is_ascii_alphanumeric() || c == '_' {
            end += c.len_utf8();
        } else {
            break;
        }
    }
    Some(s[..end].to_string())
}

/// The index of the line holding the closing `}` of the function whose
/// definition starts at `def` — a depth-only brace match (shell `${…}`
/// expansions are balanced, so they do not disturb the depth). Falls back to
/// `def` when no block opens.
fn sh_block_end(lines: &[SourceLine<'_>], def: usize) -> usize {
    let mut depth: i32 = 0;
    let mut opened = false;
    for (i, line) in lines.iter().enumerate().skip(def) {
        for ch in line.text.chars() {
            match ch {
                '{' => {
                    depth += 1;
                    opened = true;
                }
                '}' if opened => depth -= 1,
                _ => {}
            }
        }
        if opened && depth <= 0 {
            return i;
        }
    }
    def
}

/// The `yaml` emitter: every top-level (column-0) mapping key, plus each job
/// under a top-level `jobs:` and each step (list item) under a job's `steps:`.
/// Jobs nest under `jobs`; steps nest under their job and are named by their
/// ordinal (`step-1`, …) — a minimal, collision-free normalization.
fn emit_yaml(path: &Path, bytes: &[u8], id_prefix: &str, next_id: &mut u64) -> Structure {
    let text = String::from_utf8_lossy(bytes);
    let lines = source_lines(&text);
    let mut b = StructureBuilder::new(&path.to_string_lossy(), id_prefix, next_id);

    let mut i = 0usize;
    while i < lines.len() {
        let Some((indent, key)) = yaml_mapping_key(lines[i].text) else {
            i += 1;
            continue;
        };
        if indent != 0 {
            i += 1;
            continue;
        }
        let added = b.add(None, &key, lines[i].span());
        if key == "jobs" {
            let (jobs_id, jobs_fqn) = added;
            let end = yaml_block_end(&lines, i + 1);
            if let Some(job_indent) = lines[i + 1..end]
                .iter()
                .filter_map(|l| yaml_mapping_key(l.text))
                .map(|(ind, _)| ind)
                .min()
            {
                let jobs: Vec<usize> = (i + 1..end)
                    .filter(|&k| {
                        matches!(yaml_mapping_key(lines[k].text), Some((ind, _)) if ind == job_indent)
                    })
                    .collect();
                for (ji, &job_line) in jobs.iter().enumerate() {
                    let job_end = jobs.get(ji + 1).copied().unwrap_or(end);
                    let Some((_, job_key)) = yaml_mapping_key(lines[job_line].text) else {
                        continue;
                    };
                    let (job_id, job_fqn) = b.add(
                        Some((&jobs_id, &jobs_fqn)),
                        &job_key,
                        lines[job_line].span(),
                    );
                    let steps_line = (job_line + 1..job_end).find(|&k| {
                        matches!(
                            yaml_mapping_key(lines[k].text),
                            Some((ind, key)) if ind > job_indent && key == "steps"
                        )
                    });
                    if let Some(step_line) = steps_line {
                        let step_indent = yaml_mapping_key(lines[step_line].text)
                            .map(|(ind, _)| ind)
                            .unwrap_or(job_indent);
                        let mut n = 0u32;
                        for line in &lines[step_line + 1..job_end] {
                            if let Some((ind, _)) = yaml_list_item(line.text) {
                                if ind > step_indent {
                                    n += 1;
                                    b.add(
                                        Some((&job_id, &job_fqn)),
                                        &format!("step-{n}"),
                                        line.span(),
                                    );
                                }
                            } else if let Some((ind, _)) = yaml_mapping_key(line.text) {
                                if ind <= step_indent {
                                    break;
                                }
                            }
                        }
                    }
                }
            }
            i = end;
            continue;
        }
        i += 1;
    }
    b.finish()
}

/// A YAML block-mapping key on `line`: `(indent, key)`, or `None` for a blank,
/// comment, list-item or non-key line. The `:` must be followed by whitespace
/// or end-of-line (so a `http://` value does not read as a key).
fn yaml_mapping_key(line: &str) -> Option<(usize, String)> {
    let indent = line.bytes().take_while(|b| *b == b' ').count();
    let rest = &line[indent..];
    if rest.is_empty() || rest.starts_with('#') || rest.starts_with('-') {
        return None;
    }
    let colon = rest.find(':')?;
    let after = &rest[colon + 1..];
    if !(after.is_empty() || after.starts_with(' ') || after.starts_with('\t')) {
        return None;
    }
    let key = rest[..colon].trim();
    if key.is_empty() {
        return None;
    }
    Some((indent, unquote(key)))
}

/// A YAML sequence entry on `line`: `(indent, item-text)`, or `None`.
fn yaml_list_item(line: &str) -> Option<(usize, String)> {
    let indent = line.bytes().take_while(|b| *b == b' ').count();
    let rest = &line[indent..];
    let body = rest.strip_prefix('-')?;
    if !(body.is_empty() || body.starts_with(' ')) {
        return None;
    }
    Some((indent, body.trim().to_string()))
}

/// The end index (exclusive) of the block starting at `start`: the lines up to
/// the next column-0 mapping key, blank lines and comments included.
fn yaml_block_end(lines: &[SourceLine<'_>], start: usize) -> usize {
    let mut end = start;
    while end < lines.len() {
        let t = lines[end].text;
        if t.trim().is_empty() || t.trim_start().starts_with('#') {
            end += 1;
            continue;
        }
        match yaml_mapping_key(t) {
            Some((0, _)) => break,
            Some(_) => end += 1,
            None => {
                if yaml_list_item(t).is_some() {
                    end += 1;
                } else {
                    break;
                }
            }
        }
    }
    end
}

/// The `json` emitter: every key of the top-level object, in document order.
/// Nested keys are not emitted (minimal depth).
fn emit_json(path: &Path, bytes: &[u8], id_prefix: &str, next_id: &mut u64) -> Structure {
    let text = String::from_utf8_lossy(bytes);
    let lines = source_lines(&text);
    let mut b = StructureBuilder::new(&path.to_string_lossy(), id_prefix, next_id);
    for (key, offset) in json_top_level_keys(&text) {
        let line = &lines[line_index_at(&lines, offset)];
        b.add(None, &key, line.span());
    }
    b.finish()
}

/// The keys of a JSON document's top-level object, in order, each paired with
/// the byte offset of its opening quote. Empty for a non-object root.
fn json_top_level_keys(text: &str) -> Vec<(String, usize)> {
    let mut out = Vec::new();
    let mut depth: i32 = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        match c {
            '{' | '[' => depth += 1,
            '}' | ']' => depth -= 1,
            '"' => {
                let (s, end) = json_read_string(text, i);
                if depth == 1 && text[end..].trim_start().starts_with(':') {
                    out.push((s, i));
                }
                while let Some(&(j, _)) = chars.peek() {
                    if j < end {
                        chars.next();
                    } else {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Reads the JSON string whose opening quote is at `start`, returning the
/// decoded text and the index just past its closing quote.
#[allow(clippy::while_let_on_iterator)]
fn json_read_string(text: &str, start: usize) -> (String, usize) {
    let mut s = String::new();
    let mut escaped = false;
    let mut end = text.len();
    let mut iter = text[start + 1..].char_indices();
    while let Some((off, c)) = iter.next() {
        if escaped {
            match c {
                'n' => s.push('\n'),
                't' => s.push('\t'),
                'r' => s.push('\r'),
                'b' => s.push('\u{8}'),
                'f' => s.push('\u{c}'),
                'u' => {
                    let mut hex = String::new();
                    for _ in 0..4 {
                        if let Some((_, h)) = iter.next() {
                            hex.push(h);
                        }
                    }
                    if let Ok(cp) = u32::from_str_radix(&hex, 16) {
                        if let Some(ch) = char::from_u32(cp) {
                            s.push(ch);
                        }
                    }
                }
                other => s.push(other),
            }
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == '"' {
            end = start + 1 + off + 1;
            break;
        } else {
            s.push(c);
        }
    }
    (s, end)
}

/// The `toml` emitter: one `Struct` per table header (`[t]` / `[[t]]`) plus one
/// per key, nested under the table that owns it (or the file before the first
/// header).
fn emit_toml(path: &Path, bytes: &[u8], id_prefix: &str, next_id: &mut u64) -> Structure {
    let text = String::from_utf8_lossy(bytes);
    let lines = source_lines(&text);
    let mut b = StructureBuilder::new(&path.to_string_lossy(), id_prefix, next_id);
    let mut table: Option<(String, String)> = None;
    for line in &lines {
        if let Some(name) = toml_table_header(line.text) {
            table = Some(b.add(None, &name, line.span()));
        } else if let Some(key) = toml_key(line.text) {
            let parent = table.as_ref().map(|(id, fqn)| (id.as_str(), fqn.as_str()));
            b.add(parent, &key, line.span());
        }
    }
    b.finish()
}

/// A TOML table header on `line` (`[a.b]` or `[[a.b]]`) → its name, or `None`.
fn toml_table_header(line: &str) -> Option<String> {
    let s = line.trim();
    if let Some(rest) = s.strip_prefix("[[") {
        let name = rest.strip_suffix("]]")?.trim();
        return (!name.is_empty()).then(|| name.to_string());
    }
    if let Some(rest) = s.strip_prefix('[') {
        let name = rest.strip_suffix(']')?.trim();
        return (!name.is_empty()).then(|| name.to_string());
    }
    None
}

/// A TOML key assignment on `line` (`key = value`) → its key, or `None`.
fn toml_key(line: &str) -> Option<String> {
    let s = line.trim();
    if s.is_empty() || s.starts_with('#') || s.starts_with('[') {
        return None;
    }
    let eq = s.find('=')?;
    let key = s[..eq].trim();
    if key.is_empty() {
        return None;
    }
    Some(unquote(key))
}

/// The `xml` emitter: every element occurrence, nested by its position in the
/// document. Repeated sibling element names are disambiguated (`dependency`,
/// `dependency-1`, …). Comments, processing instructions, doctypes and CDATA
/// are skipped.
fn emit_xml(path: &Path, bytes: &[u8], id_prefix: &str, next_id: &mut u64) -> Structure {
    let text = String::from_utf8_lossy(bytes);
    let lines = source_lines(&text);
    let mut b = StructureBuilder::new(&path.to_string_lossy(), id_prefix, next_id);
    let mut stack: Vec<(String, String)> = Vec::new();
    let mut rest: &str = &text;
    let mut offset = 0usize;
    while let Some(lt) = rest.find('<') {
        offset += lt;
        rest = &rest[lt..];
        if let Some(after) = rest.strip_prefix("<!--") {
            match after.find("-->") {
                Some(k) => {
                    let adv = 4 + k + 3;
                    rest = &rest[adv..];
                    offset += adv;
                }
                None => break,
            }
            continue;
        }
        if let Some(after) = rest.strip_prefix("<![CDATA[") {
            match after.find("]]>") {
                Some(k) => {
                    let adv = 9 + k + 3;
                    rest = &rest[adv..];
                    offset += adv;
                }
                None => break,
            }
            continue;
        }
        if rest.starts_with("<?") || rest.starts_with("<!") {
            match rest.find('>') {
                Some(k) => {
                    let adv = k + 1;
                    rest = &rest[adv..];
                    offset += adv;
                }
                None => break,
            }
            continue;
        }
        let Some(gt) = rest.find('>') else { break };
        let inner = &rest[1..gt];
        let self_closing = inner.trim_end().ends_with('/');
        let inner = inner.trim_end().trim_end_matches('/');
        let closing = inner.starts_with('/');
        let name = inner
            .trim_start_matches('/')
            .split_whitespace()
            .next()
            .unwrap_or("");
        if !name.is_empty() {
            if closing {
                stack.pop();
            } else {
                let parent = stack.last().map(|(id, fqn)| (id.as_str(), fqn.as_str()));
                let added = b.add(parent, name, lines[line_index_at(&lines, offset)].span());
                if !self_closing {
                    stack.push(added);
                }
            }
        }
        let adv = gt + 1;
        rest = &rest[adv..];
        offset += adv;
    }
    b.finish()
}

/// The `dockerfile` emitter: one `Struct` per build stage (`FROM`, named by its
/// `AS` alias or `stage-N`) plus one per `RUN`/`COPY`/`ENV` instruction, nested
/// under the current stage. The `FROM` line is the stage record itself.
fn emit_dockerfile(path: &Path, bytes: &[u8], id_prefix: &str, next_id: &mut u64) -> Structure {
    let text = String::from_utf8_lossy(bytes);
    let lines = source_lines(&text);
    let mut b = StructureBuilder::new(&path.to_string_lossy(), id_prefix, next_id);
    let mut stage: Option<(String, String)> = None;
    let mut stage_no = 0u32;
    for line in &lines {
        let t = line.text.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let Some((kw, rest)) = dockerfile_instruction(t) else {
            continue;
        };
        match kw {
            "FROM" => {
                stage_no += 1;
                let name = dockerfile_stage_name(rest, stage_no);
                stage = Some(b.add(None, &name, line.span()));
            }
            "RUN" | "COPY" | "ENV" => {
                let parent = stage.as_ref().map(|(id, fqn)| (id.as_str(), fqn.as_str()));
                b.add(parent, kw, line.span());
            }
            _ => {}
        }
    }
    b.finish()
}

/// The instruction keyword and its arguments for a Dockerfile line, or `None`
/// for an instruction the emitter does not name.
fn dockerfile_instruction(line: &str) -> Option<(&'static str, &str)> {
    let (kw, rest) = match line.split_once(char::is_whitespace) {
        Some((k, r)) => (k, r.trim_start()),
        None => (line, ""),
    };
    let known = match kw.to_ascii_uppercase().as_str() {
        "FROM" => "FROM",
        "RUN" => "RUN",
        "COPY" => "COPY",
        "ENV" => "ENV",
        _ => return None,
    };
    Some((known, rest))
}

/// A build stage's name: its `AS <alias>` alias when present, else `stage-N`.
fn dockerfile_stage_name(rest: &str, stage_no: u32) -> String {
    let tokens: Vec<&str> = rest.split_whitespace().collect();
    if let Some(pos) = tokens.iter().position(|t| t.eq_ignore_ascii_case("AS")) {
        if let Some(alias) = tokens.get(pos + 1) {
            return alias.to_string();
        }
    }
    format!("stage-{stage_no}")
}

/// The `makefile` emitter: targets, variable assignments and include
/// directives, all file-rooted (a Makefile has no enclosing structure).
fn emit_makefile(path: &Path, bytes: &[u8], id_prefix: &str, next_id: &mut u64) -> Structure {
    let text = String::from_utf8_lossy(bytes);
    let lines = source_lines(&text);
    let mut b = StructureBuilder::new(&path.to_string_lossy(), id_prefix, next_id);
    for line in &lines {
        let t = line.text;
        if t.starts_with('\t') || t.trim().is_empty() || t.trim_start().starts_with('#') {
            continue;
        }
        if let Some(name) = makefile_include(t) {
            b.add(None, &name, line.span());
        } else if let Some(name) = makefile_assignment(t) {
            b.add(None, &name, line.span());
        } else if let Some(names) = makefile_targets(t) {
            for name in names {
                b.add(None, &name, line.span());
            }
        }
    }
    b.finish()
}

/// An include directive on `line` (`include`/`-include`/`sinclude`) → its
/// argument, or `None`.
fn makefile_include(line: &str) -> Option<String> {
    let s = line.trim_start();
    let rest = ["-include", "sinclude", "include"].iter().find_map(|kw| {
        s.strip_prefix(kw)
            .filter(|r| r.starts_with([' ', '\t']))
            .map(str::trim)
    })?;
    (!rest.is_empty()).then(|| rest.to_string())
}

/// A variable assignment on `line` (`VAR = x`, `VAR := x`, `VAR ?= x`, …) → its
/// name, or `None`.
fn makefile_assignment(line: &str) -> Option<String> {
    if line.is_empty() || line.starts_with(char::is_whitespace) || line.starts_with('#') {
        return None;
    }
    let eq = line.find('=')?;
    let name = line[..eq]
        .trim_end()
        .trim_end_matches([':', '?', '+', '!'])
        .trim_end();
    if name.is_empty() || name.contains(':') || name.contains(' ') {
        return None;
    }
    Some(name.to_string())
}

/// The targets of a rule line (`targets: prereqs`) → one name per target, or
/// `None`.
fn makefile_targets(line: &str) -> Option<Vec<String>> {
    if line.starts_with(char::is_whitespace) {
        return None;
    }
    let colon = line.find(':')?;
    if line[colon + 1..].starts_with('=') {
        return None; // `:=` assignment
    }
    let left = line[..colon].trim();
    if left.is_empty() || left.contains('=') {
        return None;
    }
    Some(left.split_whitespace().map(str::to_string).collect())
}

/// The `ini` emitter: one `Struct` per `[section]` plus one per key, nested
/// under the section that owns it (or the file before the first section).
fn emit_ini(path: &Path, bytes: &[u8], id_prefix: &str, next_id: &mut u64) -> Structure {
    let text = String::from_utf8_lossy(bytes);
    let lines = source_lines(&text);
    let mut b = StructureBuilder::new(&path.to_string_lossy(), id_prefix, next_id);
    let mut section: Option<(String, String)> = None;
    for line in &lines {
        if let Some(name) = ini_section(line.text) {
            section = Some(b.add(None, &name, line.span()));
        } else if let Some(key) = ini_key(line.text) {
            let parent = section
                .as_ref()
                .map(|(id, fqn)| (id.as_str(), fqn.as_str()));
            b.add(parent, &key, line.span());
        }
    }
    b.finish()
}

/// A `[section]` header on `line` → its name, or `None`.
fn ini_section(line: &str) -> Option<String> {
    let s = line.trim();
    let rest = s.strip_prefix('[')?;
    let name = rest.strip_suffix(']')?.trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// A `key = value` / `key: value` entry on `line` → its key, or `None`.
fn ini_key(line: &str) -> Option<String> {
    let s = line.trim();
    if s.is_empty() || s.starts_with('#') || s.starts_with(';') || s.starts_with('[') {
        return None;
    }
    let sep = s.find(['=', ':'])?;
    let key = s[..sep].trim();
    (!key.is_empty()).then(|| key.to_string())
}

/// The `misc` residual: a file no format stream (and no code frontend) claims
/// gets its `File` record only — no structure facts.
fn emit_misc(_path: &Path, _bytes: &[u8], _id_prefix: &str, _next_id: &mut u64) -> Structure {
    Structure {
        structs: Vec::new(),
        contains: Vec::new(),
    }
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let args = match parse_args(&argv) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    if let Err(e) = run(args) {
        eprintln!("structfrontend: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── shared helpers (kept at the `mod tests` root, reached by every tier) ──

    /// Runs a format emitter over `text` with a fresh id counter.
    fn emit(f: fn(&Path, &[u8], &str, &mut u64) -> Structure, path: &str, text: &str) -> Structure {
        let mut next = 1u64;
        f(Path::new(path), text.as_bytes(), "n", &mut next)
    }

    /// `(name, parent, start_line, end_line)` for every `Struct` record.
    fn struct_rows(s: &Structure) -> Vec<(String, String, u32, u32)> {
        s.structs
            .iter()
            .map(|r| match r {
                Rec::Struct {
                    parent,
                    name,
                    start_line,
                    end_line,
                    ..
                } => (name.clone(), parent.clone(), *start_line, *end_line),
                _ => unreachable!("a Structure holds only Struct records"),
            })
            .collect()
    }

    /// The `Struct` names, in emission order.
    fn names(s: &Structure) -> Vec<String> {
        struct_rows(s).into_iter().map(|(n, ..)| n).collect()
    }

    /// `id -> rendered FQN (parent.name)` for a Structure's Structs.
    fn fqns_by_id(s: &Structure) -> std::collections::HashMap<String, String> {
        s.structs
            .iter()
            .map(|r| match r {
                Rec::Struct {
                    id, parent, name, ..
                } => (id.clone(), format!("{parent}.{name}")),
                _ => unreachable!("a Structure holds only Struct records"),
            })
            .collect()
    }

    /// The `contains` edges of a Structure, resolved to `(from-fqn, to-fqn)`.
    fn contains_fqns(s: &Structure) -> Vec<(String, String)> {
        let map = fqns_by_id(s);
        s.contains
            .iter()
            .map(|r| match r {
                Rec::Contains { from, to } => (map[from].clone(), map[to].clone()),
                _ => unreachable!("a Structure holds only Contains records"),
            })
            .collect()
    }

    /// The absorbed md emitter's record half: `build_doc`'s sections as
    /// `Struct`/`contains` facts (mirrors `run`'s md branch).
    fn md_structure(doc: &Doc) -> Structure {
        Structure {
            structs: doc
                .sections
                .iter()
                .map(|s| Rec::Struct {
                    id: s.id.clone(),
                    parent: s.parent.clone(),
                    name: s.name.clone(),
                    path: doc.path.clone(),
                    start: s.start,
                    end: s.end,
                    start_line: s.start_line,
                    end_line: s.end_line,
                })
                .collect(),
            contains: doc
                .sections
                .iter()
                .filter_map(|s| {
                    s.parent_index.map(|pi| Rec::Contains {
                        from: doc.sections[pi].id.clone(),
                        to: s.id.clone(),
                    })
                })
                .collect(),
        }
    }

    /// The in-memory record assembly the `int` tests exercise: route each file
    /// through its format emitter and assemble the Module/File/Struct/contains
    /// records exactly as `run` does, serialized with the real `write_rec`.
    /// `selected` mirrors `run`'s per-stream `--stream <id>` retain. No
    /// filesystem, no process.
    fn assemble(
        base: &Path,
        files: &[(&str, &str, &str)],
        selected: Option<&str>,
    ) -> Vec<serde_json::Value> {
        let mut next_id = 1u64;
        let mut modules: BTreeSet<String> = BTreeSet::new();
        let mut file_recs: Vec<Rec> = Vec::new();
        let mut struct_recs: Vec<Rec> = Vec::new();
        let mut contains_recs: Vec<Rec> = Vec::new();
        for &(path_str, stream, text) in files {
            if selected.is_some_and(|sel| sel != stream) {
                continue;
            }
            let path = Path::new(path_str);
            let bytes = text.as_bytes();
            let module = repo_relative_dir(base, path);
            let structure = match stream {
                "md" => md_structure(&build_doc(path, bytes, base, "n", &mut next_id)),
                "sh" => emit_sh(path, bytes, "n", &mut next_id),
                "yaml" => emit_yaml(path, bytes, "n", &mut next_id),
                "json" => emit_json(path, bytes, "n", &mut next_id),
                "toml" => emit_toml(path, bytes, "n", &mut next_id),
                "xml" => emit_xml(path, bytes, "n", &mut next_id),
                "dockerfile" => emit_dockerfile(path, bytes, "n", &mut next_id),
                "makefile" => emit_makefile(path, bytes, "n", &mut next_id),
                "ini" => emit_ini(path, bytes, "n", &mut next_id),
                _ => emit_misc(path, bytes, "n", &mut next_id),
            };
            modules.insert(module.clone());
            file_recs.push(Rec::File {
                path: path_str.to_string(),
                parent: module,
                start_line: 1,
                end_line: line_count(text),
            });
            struct_recs.extend(structure.structs);
            contains_recs.extend(structure.contains);
        }
        let mut recs: Vec<Rec> = modules.into_iter().map(|fqn| Rec::Module { fqn }).collect();
        recs.extend(file_recs);
        recs.extend(struct_recs);
        recs.extend(contains_recs);
        let mut buf: Vec<u8> = Vec::new();
        for r in &recs {
            write_rec(&mut buf, r).expect("serialize record");
        }
        String::from_utf8(buf)
            .expect("utf8 jsonl")
            .lines()
            .map(|l| serde_json::from_str(l).expect("parse record"))
            .collect()
    }

    /// A fresh scratch directory for the e2e tier.
    fn scratch_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("structfrontend-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    /// Locates the built `structfrontend` binary: the compile-time
    /// `CARGO_BIN_EXE_structfrontend` when Cargo provided it, else the
    /// `structfrontend` sibling of the profile dir (walking up from the test
    /// harness executable) — the artifact `cargo build` leaves.
    fn structfrontend_bin() -> PathBuf {
        if let Some(p) = option_env!("CARGO_BIN_EXE_structfrontend") {
            let p = PathBuf::from(p);
            if p.is_file() {
                return p;
            }
        }
        let exe = std::env::current_exe().expect("current_exe");
        let name = if cfg!(windows) {
            "structfrontend.exe"
        } else {
            "structfrontend"
        };
        let mut dir = exe.parent();
        while let Some(d) = dir {
            let candidate = d.join(name);
            if candidate.is_file() {
                return candidate;
            }
            dir = d.parent();
        }
        panic!(
            "could not locate the built `structfrontend` binary from {} — run \
             `cargo build --manifest-path src/structlib/Cargo.toml` first",
            exe.display()
        );
    }

    mod unit {
        use super::*;

        #[test]
        fn sh_functions_are_extracted() {
            let src = "# a comment\n\
                       FOO=bar\n\
                       build_all() {\n\
                       \x20 echo hi\n\
                       }\n\
                       function deploy {\n\
                       \x20 true\n\
                       }\n\
                       helper () { :; }\n\
                       echo done\n";
            let s = emit(emit_sh, "build.sh", src);
            let rows = struct_rows(&s);
            assert_eq!(
                rows.iter().map(|(n, ..)| n.as_str()).collect::<Vec<_>>(),
                vec!["build_all", "deploy", "helper"]
            );
            // Ranges: the definition line through the closing brace.
            assert_eq!((rows[0].2, rows[0].3), (3, 5));
            assert_eq!((rows[1].2, rows[1].3), (6, 8));
            assert_eq!((rows[2].2, rows[2].3), (9, 9));
            assert_eq!(rows[0].1, "build.sh");
            assert!(s.contains.is_empty());
        }

        #[test]
        fn yaml_keys_jobs_and_steps_are_extracted() {
            let src = "name: CI\n\
                       on: push\n\
                       jobs:\n\
                       \x20 build:\n\
                       \x20   runs-on: ubuntu-latest\n\
                       \x20   steps:\n\
                       \x20     - uses: actions/checkout@v4\n\
                       \x20     - run: make\n\
                       \x20 test:\n\
                       \x20   steps:\n\
                       \x20     - run: make test\n\
                       permissions:\n\
                       \x20 contents: read\n";
            let s = emit(emit_yaml, "ci.yml", src);
            let rows = struct_rows(&s);
            assert_eq!(
                rows.iter().map(|(n, ..)| n.as_str()).collect::<Vec<_>>(),
                vec![
                    "name",
                    "on",
                    "jobs",
                    "build",
                    "step-1",
                    "step-2",
                    "test",
                    "step-1",
                    "permissions"
                ]
            );
            let contains = contains_fqns(&s);
            assert!(contains.contains(&("ci.yml.jobs".into(), "ci.yml.jobs.build".into())));
            assert!(contains.contains(&("ci.yml.jobs".into(), "ci.yml.jobs.test".into())));
            assert!(contains.contains(&(
                "ci.yml.jobs.build".into(),
                "ci.yml.jobs.build.step-1".into()
            )));
            assert!(contains.contains(&(
                "ci.yml.jobs.build".into(),
                "ci.yml.jobs.build.step-2".into()
            )));
            assert!(
                contains.contains(&("ci.yml.jobs.test".into(), "ci.yml.jobs.test.step-1".into()))
            );
        }

        #[test]
        fn json_keys_are_extracted() {
            let src = "{\n\
                       \x20 \"name\": \"x\",\n\
                       \x20 \"nested\": {\n\
                       \x20   \"inner\": 1\n\
                       \x20 },\n\
                       \x20 \"list\": [1, 2]\n\
                       }\n";
            let s = emit(emit_json, "data.json", src);
            let rows = struct_rows(&s);
            assert_eq!(
                rows.iter().map(|(n, ..)| n.as_str()).collect::<Vec<_>>(),
                vec!["name", "nested", "list"]
            );
            assert_eq!(rows.iter().map(|r| r.2).collect::<Vec<_>>(), vec![2, 3, 6]);
            assert!(s.contains.is_empty());
        }

        #[test]
        fn toml_tables_and_keys_are_extracted() {
            let src = "title = \"x\"\n\
                       [package]\n\
                       name = \"y\"\n\
                       version = \"1\"\n\
                       [[bin]]\n\
                       name = \"z\"\n";
            let s = emit(emit_toml, "Cargo.toml", src);
            let rows = struct_rows(&s);
            assert_eq!(
                rows.iter().map(|(n, ..)| n.as_str()).collect::<Vec<_>>(),
                vec!["title", "package", "name", "version", "bin", "name"]
            );
            assert_eq!(rows[0].1, "Cargo.toml");
            assert_eq!(rows[2].1, "Cargo.toml.package");
            assert_eq!(rows[5].1, "Cargo.toml.bin");
            let contains = contains_fqns(&s);
            assert!(contains.contains(&(
                "Cargo.toml.package".into(),
                "Cargo.toml.package.name".into()
            )));
            assert!(contains.contains(&("Cargo.toml.bin".into(), "Cargo.toml.bin.name".into())));
        }

        #[test]
        fn xml_elements_are_extracted() {
            let src = "<project>\n\
                       \x20 <dependency>a</dependency>\n\
                       \x20 <dependency>b</dependency>\n\
                       \x20 <build><plugin/></build>\n\
                       </project>\n";
            let s = emit(emit_xml, "pom.xml", src);
            let rows = struct_rows(&s);
            assert_eq!(
                rows.iter().map(|(n, ..)| n.as_str()).collect::<Vec<_>>(),
                vec!["project", "dependency", "dependency-1", "build", "plugin"]
            );
            assert_eq!((rows[1].2, rows[1].3), (2, 2));
            let contains = contains_fqns(&s);
            assert!(contains.contains(&(
                "pom.xml.project".into(),
                "pom.xml.project.dependency".into()
            )));
            assert!(contains.contains(&(
                "pom.xml.project".into(),
                "pom.xml.project.dependency-1".into()
            )));
            assert!(contains.contains(&(
                "pom.xml.project.build".into(),
                "pom.xml.project.build.plugin".into()
            )));
        }

        #[test]
        fn dockerfile_stages_and_instructions_are_extracted() {
            let src = "FROM rust:1.98 AS builder\n\
                       RUN cargo build --release\n\
                       COPY . .\n\
                       ENV RUST_LOG=info\n\
                       FROM debian:bookworm\n\
                       RUN apt-get update\n";
            let s = emit(emit_dockerfile, "Dockerfile", src);
            let rows = struct_rows(&s);
            assert_eq!(
                rows.iter().map(|(n, ..)| n.as_str()).collect::<Vec<_>>(),
                vec!["builder", "RUN", "COPY", "ENV", "stage-2", "RUN"]
            );
            let contains = contains_fqns(&s);
            assert!(
                contains.contains(&("Dockerfile.builder".into(), "Dockerfile.builder.RUN".into()))
            );
            assert!(contains.contains(&(
                "Dockerfile.builder".into(),
                "Dockerfile.builder.COPY".into()
            )));
            assert!(
                contains.contains(&("Dockerfile.builder".into(), "Dockerfile.builder.ENV".into()))
            );
            assert!(
                contains.contains(&("Dockerfile.stage-2".into(), "Dockerfile.stage-2.RUN".into()))
            );
        }

        #[test]
        fn makefile_targets_variables_and_includes_are_extracted() {
            let src = "CC := cc\n\
                       CFLAGS = -O2\n\
                       include common.mk\n\
                       all: build test\n\
                       \techo done\n\
                       build:\n\
                       \t$(CC) -o build main.c\n";
            let s = emit(emit_makefile, "Makefile", src);
            assert_eq!(names(&s), vec!["CC", "CFLAGS", "common.mk", "all", "build"]);
            assert!(s.contains.is_empty());
        }

        #[test]
        fn ini_sections_and_keys_are_extracted() {
            let src = "root_key = 1\n\
                       [server]\n\
                       host = localhost\n\
                       port = 8080\n\
                       [client]\n\
                       timeout: 30\n";
            let s = emit(emit_ini, "app.ini", src);
            let rows = struct_rows(&s);
            assert_eq!(
                rows.iter().map(|(n, ..)| n.as_str()).collect::<Vec<_>>(),
                vec!["root_key", "server", "host", "port", "client", "timeout"]
            );
            let contains = contains_fqns(&s);
            assert!(contains.contains(&("app.ini.server".into(), "app.ini.server.host".into())));
            assert!(contains.contains(&("app.ini.client".into(), "app.ini.client.timeout".into())));
        }

        #[test]
        fn md_heading_parser_predicates() {
            let text = "# Title\n\
                        \x20   # indented (4 spaces)\n\
                        ## Sub ##\n\
                        ####### seven\n\
                        ```\n\
                        # fenced\n\
                        ```\n\
                        ### Deep\n";
            let hs = parse_headings(text);
            assert_eq!(
                hs.iter().map(|h| h.level).collect::<Vec<_>>(),
                vec![1, 2, 3]
            );
            assert_eq!(
                hs.iter().map(|h| h.text.as_str()).collect::<Vec<_>>(),
                vec!["Title", "Sub", "Deep"]
            );
            assert_eq!(
                hs.iter().map(|h| h.start_line).collect::<Vec<_>>(),
                vec![1, 3, 8]
            );
        }

        #[test]
        fn md_line_count_edge_cases() {
            assert_eq!(line_count(""), 1);
            assert_eq!(line_count("a"), 1);
            assert_eq!(line_count("a\n"), 1);
            assert_eq!(line_count("a\nb"), 2);
            assert_eq!(line_count("a\nb\n"), 2);
        }

        #[test]
        fn claim_and_scope_predicates() {
            // The complete extension→stream routing, including the code-extension
            // skip and the residual `misc`. The skip list is the UNION of every
            // shipped code frontend's extensions: the C++ extras
            // (`.cxx/.c++/.hpp/.hh/.hxx/.tpp/.ipp`) and the unified JS/TS module
            // variants (`.mts/.cts/.mjs/.cjs`) are code-claimed too, while `.c`
            // stays residual `misc` (the C++ frontend does not claim it).
            let code = [
                "rs", "go", "java", "cpp", "cc", "cxx", "c++", "h", "hpp", "hh", "hxx", "tpp",
                "ipp", "ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs", "cs", "py", "pyi",
            ];
            for ext in code {
                assert_eq!(
                    stream_for_path(Path::new(&format!("src/file.{ext}"))),
                    None,
                    "code extension .{ext} must be skipped"
                );
            }
            let routed = [
                ("a.md", Some("md")),
                ("a.markdown", Some("md")),
                ("a.sh", Some("sh")),
                ("a.bash", Some("sh")),
                ("a.yaml", Some("yaml")),
                ("a.yml", Some("yaml")),
                ("a.json", Some("json")),
                ("package-lock.json", Some("json")),
                ("a.toml", Some("toml")),
                ("Cargo.lock", Some("toml")),
                ("a.xml", Some("xml")),
                ("Dockerfile", Some("dockerfile")),
                ("Dockerfile.dev", Some("dockerfile")),
                ("Makefile", Some("makefile")),
                ("GNUmakefile", Some("makefile")),
                ("rules.mk", Some("makefile")),
                ("a.ini", Some("ini")),
                ("a.cfg", Some("ini")),
                (".editorconfig", Some("ini")),
                (".env.local", Some("ini")),
                ("LICENSE", Some("misc")),
                (".gitignore", Some("misc")),
                ("a.c", Some("misc")),
            ];
            for (path, want) in routed {
                assert_eq!(stream_for_path(Path::new(path)), want, "routing for {path}");
            }

            // Pruned trees and the excluded-tree-below-root rule.
            assert!(is_pruned_dir("target"));
            assert!(is_pruned_dir("node_modules"));
            assert!(is_pruned_dir(".git"));
            assert!(is_pruned_dir(".worktrees"));
            assert!(!is_pruned_dir("src"));
            let root = Path::new("/repo");
            assert!(under_excluded_tree(Path::new("/repo/target/a.sh"), root));
            assert!(under_excluded_tree(
                Path::new("/repo/node_modules/x/a.sh"),
                root
            ));
            assert!(!under_excluded_tree(Path::new("/repo/src/a.sh"), root));
            // A scan root that itself lives under `.worktrees/` is still scanned.
            let wt = Path::new("/repo/.worktrees/proj");
            assert!(!under_excluded_tree(wt, wt));

            // `--exclude-path` substrings and the config-scope include/exclude globs.
            let scope = StructuralScope {
                include: vec!["src/**".into()],
                exclude: vec!["src/gen/**".into()],
                code_type: None,
            };
            let base = Path::new("/repo");
            assert!(in_scope(
                Path::new("/repo/src/a.sh"),
                root,
                base,
                &[],
                &[],
                &scope
            ));
            assert!(!in_scope(
                Path::new("/repo/src/gen/a.sh"),
                root,
                base,
                &[],
                &[],
                &scope
            ));
            assert!(!in_scope(
                Path::new("/repo/docs/a.md"),
                root,
                base,
                &[],
                &[],
                &scope
            ));
            assert!(!in_scope(
                Path::new("/repo/src/a.sh"),
                root,
                base,
                &[],
                &["src".into()],
                &scope
            ));
            assert!(!in_scope(
                Path::new("/repo/target/a.sh"),
                root,
                base,
                &[],
                &[],
                &scope
            ));
            let modules = vec![PathBuf::from("/repo/src")];
            assert!(in_scope(
                Path::new("/repo/src/a.sh"),
                root,
                base,
                &modules,
                &[],
                &StructuralScope::default()
            ));
            assert!(!in_scope(
                Path::new("/repo/docs/a.md"),
                root,
                base,
                &modules,
                &[],
                &StructuralScope::default()
            ));

            // glob_match
            assert!(glob_match("**/*.md", "docs/a.md"));
            assert!(glob_match("a?c", "abc"));
            assert!(glob_match("*.md", "a.md"));
            assert!(!glob_match("*.md", "a.txt"));
        }
    }

    mod int {
        use super::*;

        #[test]
        fn claimed_files_stream_file_and_structure_records() {
            let files: [(&str, &str, &str); 8] = [
                ("build.sh", "sh", "build_all() {\n  echo hi\n}\n"),
                (
                    "config.yaml",
                    "yaml",
                    "name: CI\njobs:\n  build:\n    steps:\n      - run: make\n",
                ),
                ("data.json", "json", "{\n  \"name\": \"x\"\n}\n"),
                ("Cargo.toml", "toml", "[package]\nname = \"x\"\n"),
                (
                    "pom.xml",
                    "xml",
                    "<project>\n  <name>x</name>\n</project>\n",
                ),
                (
                    "Dockerfile",
                    "dockerfile",
                    "FROM rust:1.98 AS builder\nRUN cargo build\n",
                ),
                ("Makefile", "makefile", "all:\n\techo hi\n"),
                ("app.ini", "ini", "[server]\nport = 8080\n"),
            ];
            let recs = assemble(Path::new(""), &files, None);

            // Every File record is well-formed: repo-relative module parent
            // (empty at the repo root) and line 1..line_count.
            for (path, _, text) in files {
                let f = recs
                    .iter()
                    .find(|r| r["type"] == "file" && r["path"] == path)
                    .unwrap_or_else(|| panic!("no File record for {path}"));
                assert_eq!(f["parent"], "");
                assert_eq!(f["start_line"].as_u64(), Some(1));
                assert_eq!(f["end_line"].as_u64(), Some(line_count(text) as u64));
            }

            // Each format's Struct records carry the format's named structure.
            let expected: Vec<(&str, Vec<&str>)> = vec![
                ("build.sh", vec!["build_all"]),
                ("config.yaml", vec!["name", "jobs", "build", "step-1"]),
                ("data.json", vec!["name"]),
                ("Cargo.toml", vec!["package", "name"]),
                ("pom.xml", vec!["project", "name"]),
                ("Dockerfile", vec!["builder", "RUN"]),
                ("Makefile", vec!["all"]),
                ("app.ini", vec!["server", "port"]),
            ];
            for (path, want) in expected {
                let got: Vec<&str> = recs
                    .iter()
                    .filter(|r| r["type"] == "struct" && r["path"] == path)
                    .map(|r| r["name"].as_str().unwrap())
                    .collect();
                assert_eq!(got, want, "struct names for {path}");
            }

            // Struct records are complete, and every contains edge references an
            // emitted struct id.
            let ids: std::collections::HashSet<&str> = recs
                .iter()
                .filter(|r| r["type"] == "struct")
                .map(|r| r["id"].as_str().unwrap())
                .collect();
            for r in recs.iter().filter(|r| r["type"] == "struct") {
                for field in [
                    "id",
                    "parent",
                    "name",
                    "path",
                    "start",
                    "end",
                    "start_line",
                    "end_line",
                ] {
                    assert!(r.get(field).is_some(), "struct missing {field}: {r}");
                }
            }
            for r in recs.iter().filter(|r| r["type"] == "contains") {
                assert!(
                    ids.contains(r["from"].as_str().unwrap()),
                    "dangling from: {r}"
                );
                assert!(ids.contains(r["to"].as_str().unwrap()), "dangling to: {r}");
            }
        }

        #[test]
        fn misc_residual_emits_file_only() {
            let files: [(&str, &str, &str); 2] = [
                ("LICENSE", "misc", "MIT License\n"),
                (".gitignore", "misc", "target/\n"),
            ];
            let recs = assemble(Path::new(""), &files, None);
            let types: Vec<&str> = recs.iter().map(|r| r["type"].as_str().unwrap()).collect();
            // One Module (deduped at the repo root) + two Files, nothing else.
            assert_eq!(types, vec!["module", "file", "file"]);
            assert!(recs
                .iter()
                .all(|r| r["type"] != "struct" && r["type"] != "contains"));
            let paths: Vec<&str> = recs
                .iter()
                .filter(|r| r["type"] == "file")
                .filter_map(|r| r["path"].as_str())
                .collect();
            assert_eq!(paths, vec!["LICENSE", ".gitignore"]);
        }

        #[test]
        fn md_build_doc_fact_set() {
            let base = Path::new("/repo");
            let mut next = 1u64;
            let text = "# Title\n## Sub\n## Sub\n# Title\n";
            let doc = build_doc(
                Path::new("/repo/docs/a.md"),
                text.as_bytes(),
                base,
                "n",
                &mut next,
            );
            assert_eq!(doc.path, "/repo/docs/a.md");
            assert_eq!(doc.dir, "docs");
            assert_eq!(
                doc.sections
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>(),
                vec!["title", "sub", "sub-1", "title-1"]
            );
            // Nesting: the sub-sections hang under the first title, the second
            // title re-roots at the file.
            let structure = md_structure(&doc);
            assert_eq!(names(&structure), vec!["title", "sub", "sub-1", "title-1"]);
            let contains = contains_fqns(&structure);
            assert!(contains.contains(&(
                "/repo/docs/a.md.title".into(),
                "/repo/docs/a.md.title.sub".into()
            )));
            assert!(contains.contains(&(
                "/repo/docs/a.md.title".into(),
                "/repo/docs/a.md.title.sub-1".into()
            )));
            // A repo-root document renders an EMPTY module identity (the ingestor
            // renders it as the bare `md.` root).
            let mut next = 1u64;
            let root_doc = build_doc(Path::new("/repo/README.md"), b"# T\n", base, "n", &mut next);
            assert_eq!(root_doc.dir, "");
        }

        #[test]
        fn walk_claim_stream_wiring() {
            // (a) The claim/scope classifier routes each path to exactly one
            // stream id; a code-extension file is never claimed.
            let routed = [
                ("src/main.rs", None),
                ("src/app.go", None),
                ("src/lib.ts", None),
                ("src/widget.hpp", None),
                ("src/esm.mjs", None),
                ("README.md", Some("md")),
                ("scripts/build.sh", Some("sh")),
                ("ci.yml", Some("yaml")),
                ("package.json", Some("json")),
                ("Cargo.toml", Some("toml")),
                ("pom.xml", Some("xml")),
                ("Dockerfile", Some("dockerfile")),
                ("Makefile", Some("makefile")),
                ("app.ini", Some("ini")),
                ("LICENSE", Some("misc")),
                (".gitignore", Some("misc")),
                ("Cargo.lock", Some("toml")),
                ("package-lock.json", Some("json")),
            ];
            for (path, want) in routed {
                assert_eq!(stream_for_path(Path::new(path)), want, "routing for {path}");
            }

            // The config scope narrows the claim and a pruned tree is never claimed.
            let scope = StructuralScope {
                include: vec!["src/**".into()],
                exclude: vec!["src/gen/**".into()],
                code_type: None,
            };
            let root = Path::new("/repo");
            assert!(in_scope(
                Path::new("/repo/src/a.sh"),
                root,
                root,
                &[],
                &[],
                &scope
            ));
            assert!(!in_scope(
                Path::new("/repo/target/a.sh"),
                root,
                root,
                &[],
                &[],
                &scope
            ));
            assert!(!in_scope(
                Path::new("/repo/src/gen/a.sh"),
                root,
                root,
                &[],
                &[],
                &scope
            ));

            // (b) A per-stream `--stream <id>` selection emits only that stream's
            // records; the full walk emits the union.
            let files: [(&str, &str, &str); 3] = [
                ("a.sh", "sh", "foo() { :; }\n"),
                ("b.yaml", "yaml", "key: 1\n"),
                ("c.ini", "ini", "[s]\nk = 1\n"),
            ];
            let full = assemble(root, &files, None);
            let sh_only = assemble(root, &files, Some("sh"));
            let full_files: Vec<&str> = full
                .iter()
                .filter(|r| r["type"] == "file")
                .filter_map(|r| r["path"].as_str())
                .collect();
            assert_eq!(full_files, vec!["a.sh", "b.yaml", "c.ini"]);
            let sh_files: Vec<&str> = sh_only
                .iter()
                .filter(|r| r["type"] == "file")
                .filter_map(|r| r["path"].as_str())
                .collect();
            assert_eq!(sh_files, vec!["a.sh"]);
            assert!(sh_only
                .iter()
                .all(|r| r["type"] != "file" || r["path"] == "a.sh"));
            // The union's struct set is the per-stream sets concatenated.
            assert_eq!(full.iter().filter(|r| r["type"] == "struct").count(), 4);
            assert_eq!(sh_only.iter().filter(|r| r["type"] == "struct").count(), 1);
        }
    }

    mod e2e {
        use super::*;

        #[test]
        #[ignore = "e2e tier: spawns the built structfrontend against a scratch /tmp dir; run via cargo test-e2e"]
        fn structfrontend_emits_claimed_files() {
            let dir = std::fs::canonicalize(scratch_dir("e2e")).expect("canonicalize scratch dir");
            let files: [(&str, &str); 9] = [
                ("build.sh", "build_all() {\n  echo hi\n}\n"),
                (
                    "config.yaml",
                    "name: CI\njobs:\n  build:\n    steps:\n      - run: make\n",
                ),
                ("data.json", "{\n  \"name\": \"x\"\n}\n"),
                ("Cargo.toml", "[package]\nname = \"x\"\n"),
                ("pom.xml", "<project>\n  <name>x</name>\n</project>\n"),
                ("Dockerfile", "FROM rust:1.98 AS builder\nRUN cargo build\n"),
                ("Makefile", "all:\n\techo hi\n"),
                ("app.ini", "[server]\nport = 8080\n"),
                ("README.md", "# Title\n\n## Section\n"),
            ];
            for (name, body) in files {
                std::fs::write(dir.join(name), body).expect("write fixture");
            }

            let out = std::process::Command::new(structfrontend_bin())
                .arg(&dir)
                .output()
                .expect("spawn structfrontend");
            assert!(
                out.status.success(),
                "structfrontend failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stdout = String::from_utf8(out.stdout).expect("utf8 stdout");
            let recs: Vec<serde_json::Value> = stdout
                .lines()
                .map(|l| serde_json::from_str(l).expect("jsonl record"))
                .collect();

            // One File record per claimed tracked file.
            let file_paths: Vec<&str> = recs
                .iter()
                .filter(|r| r["type"] == "file")
                .filter_map(|r| r["path"].as_str())
                .collect();
            assert_eq!(
                file_paths.len(),
                9,
                "one File record per claimed file: {file_paths:?}"
            );
            for (name, _) in files {
                let want = dir.join(name);
                assert!(
                    file_paths.iter().any(|p| Path::new(p) == want.as_path()),
                    "missing File record for {name}"
                );
            }

            // The repo-root module identity is the empty repo-relative identity
            // the ingestor renders as the bare `md.` root — never the checkout
            // basename.
            assert!(
                recs.iter().any(|r| r["type"] == "module" && r["fqn"] == ""),
                "root module identity must be empty: {recs:?}"
            );

            // The md heading Structs are unchanged: file-rooted parent + slugs.
            let readme = dir.join("README.md");
            let md_structs: Vec<&str> = recs
                .iter()
                .filter(|r| {
                    r["type"] == "struct"
                        && r["path"]
                            .as_str()
                            .is_some_and(|p| Path::new(p) == readme.as_path())
                })
                .filter_map(|r| r["name"].as_str())
                .collect();
            assert_eq!(
                md_structs,
                vec!["title", "section"],
                "md heading Structs unchanged"
            );

            std::fs::remove_dir_all(&dir).ok();
        }
    }
}
