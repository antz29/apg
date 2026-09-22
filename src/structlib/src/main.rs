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
/// code frontend parses stays that frontend's. `.c` is deliberately absent —
/// only the extensions below are code-claimed, so an unlisted `.c` file falls
/// to the residual `misc` stream.
const CODE_EXTENSIONS: &[&str] = &[
    "rs", "go", "java", "cpp", "cc", "h", "ts", "tsx", "js", "jsx", "cs", "py", "pyi",
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
    end_line: u32,
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
        end_line: line_count(&text),
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
