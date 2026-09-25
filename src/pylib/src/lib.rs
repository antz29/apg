// apg Python scanner frontend (`pyfrontend`).
//
// Exact-fidelity tier (like the Go/Java/Rust/TypeScript/C# frontends): parses
// and type-checks the project with Astral's `ty` type checker engine (the
// same engine that powers the `ty` CLI and LSP, used by Ruff/Pylance-class
// tooling), so Calls/Uses edges land on the real resolved declaration.
// Anything `ty` cannot resolve statically (dynamic dispatch via `getattr`,
// metaclass magic, etc.) becomes an `unresolved_call` / `unresolved_use` edge
// with a category, never a fabricated FQN — the same safety valve every other
// frontend has.
//
// `ty_ide`/`ty_project` are internal (`publish = false`) crates of the Ruff
// monorepo, pulled straight from the `astral-sh/ruff` repository at a pinned
// release tag (see src/pylib/Cargo.toml), the same pattern as the Rust
// frontend's rust-analyzer pin.
//
// Module model: the identity boundary is the REPO BASE — the nearest
// `.git`-bearing ancestor of the scan root (the git toplevel; a directory in a
// normal checkout, a file in a linked worktree), or the scan root itself when
// the tree is not a git checkout — never the scan root. `<repo>` and
// `<repo>/subdir` scans therefore mint the same identity for the same file
// (`requirements.requirement.portable-graph-identity`,
// `domain.value.module-identity`). Every directory between the base and the
// file contributes a dotted component — an `__init__.py`/`__init__.pyi`-bearing
// regular package and a PEP-420 namespace directory alike — and the base itself
// is the import root and never contributes its own name, so no module is named
// after the checkout/scan-root directory. The frontend emits the DOTTED
// IDENTITY VERBATIM (`pkg`, `pkg.sub`, `foo`) with NO `py.` prefix: the `py.`
// language root is applied INGESTOR-SIDE, so a frontend-baked root would
// double-root. FQNs:
//
//   pkg/sub/__init__.py         -> module `pkg.sub`
//   pkg/sub/mod.py              -> module `pkg.sub.mod`
//   pkg/sub/mod.py class Foo    -> struct `pkg.sub.mod.Foo`
//   pkg/sub/mod.py Foo.method   -> function `pkg.sub.mod.Foo.method`
//
// start/end are 0-based UTF-8 byte offsets (Ruff's native `TextSize`);
// start_line/end_line are 1-based inclusive line numbers.
//
// Discovery (task-3): `.py`/`.pyi` are accepted under the `py` language id
// (`.pyx` is NOT accepted). The frontend's own walk never descends into a
// non-project tree — `__pycache__`, `.venv`, `venv`, `.tox`, `.mypy_cache`,
// `.pytest_cache`, `.ruff_cache`, `site-packages`, `.eggs`, `*.egg-info`,
// `node_modules`, and every hidden dot entry (`.git`/`.hg`/…) — so no file
// under a venv/site-packages tree ever becomes a code node. That same tree is
// still READ as a ty resolution input (task-4).
//
// Environment (task-4): the ty project context is built from FILESYSTEM
// MARKERS only — a uv project (`pyproject.toml`/`uv.lock`), a virtualenv
// (`pyvenv.cfg`), or a bare src root. Detection never probes for a Python
// interpreter and never shells out to `python`/`uv`, so a scan works with NO
// Python runtime present. The excluded site-packages/venv tree is a
// RESOLUTION INPUT ONLY, never a scan root.
//
// Unresolved category precedence (task-6, first match wins): a project symbol
// resolves to a real Calls/Uses edge; a `ty` tool-bundled vendored typeshed
// file is `stdlib`; dependency code (a project `vendor`/`third_party` tree or
// an installed `System`/`SystemVirtual` environment such as a `.venv`
// site-packages) is `external`; in-root unbound/dynamic residue is `unknown`.
// A reference is never guessed into a fabricated FQN or edge.
//
// Incremental contract (task-7, the pinned phase-02 task-9 hand-off):
// `--targets <file>` is a UTF-8 newline-delimited list of absolute source
// paths. It is an EMISSION filter at package/module granularity: every target
// is resolved against the FULL ty project context, but only the target
// modules' per-file facts are emitted. Module records are GLOBAL scaffolding
// (they carry no location) and are emitted verbatim for the whole project so
// the incremental graph equals a full scan; an edge whose target is outside
// the emitted set carries that target's canonical FQN instead of an opaque id,
// which the ingestor's cached-fact splice resolves against the reused unit.
// `--cache-dir`/`--cache-key` locate `<cache-dir>/py/<cache-key>/`: ty
// resolves through an in-process salsa database, so (exactly as the Rust
// frontend does for rust-analyzer) the directory carries no separate on-disk
// compiler cache and creating it keeps the shared store's Python location
// keyed by the global cache key. Cross-scan fact reuse rides the shared
// content-addressed per-file fact store, and a fact unit is reusable only when
// the file's bytes AND its resolution inputs are unchanged.
//
// Usage: pyfrontend <dir> [--module <dir>]... [--id-prefix <p>]
//        [--targets <file>] [--cache-dir <dir>] [--cache-key <key>] [exclude...]

use std::collections::{HashMap, HashSet};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};

use ruff_db::files::{File, FilePath, system_path_to_file};
use ruff_db::parsed::parsed_module;
use ruff_db::source::line_index;
use ruff_db::system::{OsSystem, SystemPathBuf};
use ruff_python_ast::{self as ast, Stmt};
use ruff_text_size::{Ranged, TextSize};
use ty_ide::{CallHierarchyItem, goto_definition, outgoing_calls};
use ty_project::{Db as _, ProjectDatabase, ProjectMetadata};
use ty_python_core::ProgramFile;

/// Directory names never descended into by the frontend's own discovery walk
/// (`domain.constraint.python-exclusions`, task-3). Hidden dot entries
/// (`.venv`, `.tox`, `.git`, `.hg`, `.mypy_cache`, `.pytest_cache`,
/// `.ruff_cache`, `.eggs`, …) are covered by the general hidden-name rule;
/// `*.egg-info` by the suffix rule. The same trees are still ty resolution
/// inputs (task-4) — this is a discovery exclusion, not a resolution one.
const EXCLUDED_DIR_NAMES: &[&str] = &[
    "__pycache__",
    ".venv",
    "venv",
    ".tox",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
    "site-packages",
    ".eggs",
    "node_modules",
];

/// Dependency-environment directory names (task-6): a resolved reference
/// landing under one of these is dependency code → `external`, never
/// `unknown`. `*.egg-info` is matched by suffix.
const DEPENDENCY_DIR_NAMES: &[&str] = &[
    "vendor",
    "third_party",
    "thirdparty",
    "site-packages",
    ".venv",
    "venv",
    ".tox",
    ".eggs",
    "node_modules",
    "__pycache__",
];

/// The environment shape auto-detected from FILESYSTEM MARKERS only (task-4):
/// a uv project (`pyproject.toml`/`uv.lock`), a virtualenv (`pyvenv.cfg`), or
/// a bare src root. No interpreter probe, no `python`/`uv` shell-out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProjectKind {
    Uv,
    VirtualEnv,
    BareSrc,
}

/// Which kind of file a `ty`-resolved reference landed in. Drives the
/// unresolved category (task-6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PathOrigin {
    /// The tool-bundled vendored typeshed → `stdlib`.
    Vendored,
    /// A real file on disk → `external`/`unknown` by location.
    System,
    /// A virtual installed-environment file → `external`.
    SystemVirtual,
}

/// A parse/discovery decision: is the frontend's own walk allowed into `name`?
fn is_excluded_dir_name(name: &str) -> bool {
    name.starts_with('.') || EXCLUDED_DIR_NAMES.contains(&name) || name.ends_with(".egg-info")
}

/// True when any path component names a dependency/installed-environment tree.
fn under_dependency_tree(path: &Path) -> bool {
    path.components().any(|c| match c {
        Component::Normal(s) => {
            let s = s.to_string_lossy();
            DEPENDENCY_DIR_NAMES.contains(&s.as_ref()) || s.ends_with(".egg-info")
        }
        _ => false,
    })
}

/// The unresolved category for a `ty`-resolved reference outside our own
/// declaration set (task-6, first match wins). `Vendored` (bundled typeshed)
/// is `stdlib`; a virtual installed environment is `external`; a real file
/// under a dependency tree (project `vendor`/`third_party` or a `.venv`
/// site-packages) is `external`; elsewhere under the scan root is in-root
/// unbound/dynamic residue → `unknown`; anywhere else unambiguously external.
fn classify_unresolved(origin: PathOrigin, path: &Path, root: &Path) -> &'static str {
    match origin {
        PathOrigin::Vendored => "stdlib",
        PathOrigin::SystemVirtual => "external",
        PathOrigin::System => {
            if under_dependency_tree(path) {
                "external"
            } else if path.starts_with(root) {
                "unknown"
            } else {
                "external"
            }
        }
    }
}

/// The environment shape from the marker booleans (pure seam for task-4).
fn classify_project_markers(pyproject: bool, uv_lock: bool, pyvenv: bool) -> ProjectKind {
    if pyproject || uv_lock {
        ProjectKind::Uv
    } else if pyvenv {
        ProjectKind::VirtualEnv
    } else {
        ProjectKind::BareSrc
    }
}

/// Filesystem-marker environment detection (task-4). No interpreter probe.
fn detect_project_kind(root: &Path) -> ProjectKind {
    let has_pyproject = root.join("pyproject.toml").is_file();
    let has_uv_lock = root.join("uv.lock").is_file();
    let has_pyvenv = root.join("pyvenv.cfg").is_file()
        || root.join(".venv").join("pyvenv.cfg").is_file()
        || root.join("venv").join("pyvenv.cfg").is_file();
    classify_project_markers(has_pyproject, has_uv_lock, has_pyvenv)
}

/// Dotted module identity from the file's stem and its init-bearing ancestor
/// directory names in NEAREST-FIRST order. `__init__.py`/`__init__.pyi`
/// contribute only their package path; every other file appends its stem.
fn dotted_identity(is_init: bool, stem: &str, ancestors_nearest_first: &[String]) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !is_init {
        parts.push(stem.to_string());
    }
    parts.extend(ancestors_nearest_first.iter().cloned());
    parts.reverse();
    if parts.is_empty() {
        stem.to_string()
    } else {
        parts.join(".")
    }
}

/// Canonical FQN for a function exactly as the ingestor renders it (SPEC §4):
/// `parent.name`, or `parent.name(T1,T2,...)` when its `(parent, name)` group
/// is overloaded (the `()` form is retained for an empty param list).
fn canonical_function_fqn(parent: &str, name: &str, params: &[String], overloaded: bool) -> String {
    if overloaded {
        let joined = params.join(",");
        format!("{parent}.{name}({joined})")
    } else {
        format!("{parent}.{name}")
    }
}

/// Canonical FQN for a class: always `parent.name`.
fn canonical_struct_fqn(parent: &str, name: &str) -> String {
    format!("{parent}.{name}")
}

/// Maps an impact-set of absolute target paths to their module identities
/// (pure seam for task-7's package/module-granularity emission filter).
fn identities_from_targets<F>(targets: &[PathBuf], identity_of: F) -> HashSet<String>
where
    F: Fn(&Path) -> String,
{
    targets.iter().map(|p| identity_of(p.as_path())).collect()
}

/// One declared struct/function, with enough info to emit its record and to
/// later resolve edges pointing at it.
struct Decl {
    id: String,
    kind: DeclKind,
    /// Enclosing scope FQN (`parent.name` components), used for the canonical
    /// FQN and for the overload grouping.
    parent: String,
    name: String,
    params: Vec<String>,
    /// Canonical FQN as the ingestor renders it (filled after collection over
    /// the FULL declaration set).
    canonical: String,
    file: File,
    /// Byte offset of the declaration's name identifier — the offset ty's
    /// call-hierarchy / goto-definition APIs use to identify + resolve it.
    name_offset: TextSize,
    /// Whether this declaration's node record belongs to the emitted stream
    /// (the target set, or every declaration when no filter is in force).
    emit: bool,
}

#[derive(PartialEq, Clone, Copy)]
enum DeclKind {
    Struct,
    Function,
}

struct Scanner {
    root: PathBuf,
    module_dirs: Vec<PathBuf>,
    excludes: Vec<String>,
    id_prefix: String,
    next_id: u64,
    seen_modules: HashSet<String>,
    seen_unresolved: HashSet<String>,
    /// Struct FQN -> opaque id, so a nested function/class parented at a
    /// struct's FQN can emit an explicit Struct->{Struct,Function} contains
    /// edge (File->{Struct,Function} is derived ingestor-side from `path`;
    /// Module->File likewise from the file's `parent`).
    struct_id_by_fqn: HashMap<String, String>,
    /// (File, name-identifier-offset) -> our opaque id, used to map a
    /// `CallHierarchyItem`/`NavigationTarget` ty resolves a call/reference to
    /// back to the declaration we already emitted. Covers the FULL context.
    decl_by_pos: HashMap<(File, u32), String>,
    all_decls: Vec<Decl>,
    /// id -> canonical FQN, for every collected declaration (full context).
    id_canonical: HashMap<String, String>,
    /// The ids whose node records are actually part of the emitted stream.
    emitted_id: HashSet<String>,
    /// The module identities selected for emission, or `None` for "no filter"
    /// (a byte-identical full scan).
    target_identities: Option<HashSet<String>>,
}

impl Scanner {
    fn new(
        root: PathBuf,
        module_dirs: Vec<PathBuf>,
        excludes: Vec<String>,
        id_prefix: String,
        target_identities: Option<HashSet<String>>,
    ) -> Self {
        Scanner {
            root,
            module_dirs,
            excludes,
            id_prefix,
            next_id: 1,
            seen_modules: HashSet::new(),
            seen_unresolved: HashSet::new(),
            struct_id_by_fqn: HashMap::new(),
            decl_by_pos: HashMap::new(),
            all_decls: Vec::new(),
            id_canonical: HashMap::new(),
            emitted_id: HashSet::new(),
            target_identities,
        }
    }

    fn next_id(&mut self) -> String {
        let id = format!("{}{}", self.id_prefix, self.next_id);
        self.next_id += 1;
        id
    }

    fn is_path_excluded(&self, path: &Path) -> bool {
        let s = path.to_string_lossy();
        self.excludes.iter().any(|pat| s.contains(pat.as_str()))
    }

    /// The endpoint to emit for an edge target: the opaque id when the
    /// declaration belongs to the emitted stream, else its canonical FQN (the
    /// cached-fact splice resolves that against the reused unit). With no
    /// filter in force every id is emitted, so this is always `id`.
    fn endpoint(&self, id: &str) -> String {
        if self.emitted_id.contains(id) {
            id.to_string()
        } else {
            self.id_canonical
                .get(id)
                .cloned()
                .unwrap_or_else(|| id.to_string())
        }
    }

    /// Discovers every `.py`/`.pyi` file under the scan roots (the project
    /// root, or the `--module` dirs when given), skipping non-project
    /// directories and any CLI-excluded path.
    fn discover_files(&self) -> Vec<PathBuf> {
        let roots: Vec<PathBuf> = if self.module_dirs.is_empty() {
            vec![self.root.clone()]
        } else {
            self.module_dirs.clone()
        };
        let mut files = Vec::new();
        for r in &roots {
            self.walk_dir(r, &mut files);
        }
        files.sort();
        files.dedup();
        files
    }

    fn walk_dir(&self, dir: &Path, out: &mut Vec<PathBuf>) {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if is_excluded_dir_name(&name) {
                continue;
            }
            if self.is_path_excluded(&path) {
                continue;
            }
            if path.is_dir() {
                self.walk_dir(&path, out);
            } else if path.extension().is_some_and(|e| e == "py" || e == "pyi") {
                out.push(path);
            }
        }
    }

    /// Computes the dotted module identity for a source file from its full
    /// package directory path, relative to the REPO BASE (the git toplevel
    /// found by walking up from the scan root, or the scan root when the tree
    /// is not a git checkout): every directory between the base and the file
    /// is a package-or-PEP-420-namespace component (a directory with no
    /// `__init__.py` is a namespace package and contributes its name just like
    /// an `__init__.py`/`__init__.pyi`-bearing regular package). The base is
    /// the import root and never contributes its own name, so no module is
    /// named after the checkout/scan-root directory — and because the boundary
    /// is the repo base, `<repo>` and `<repo>/subdir` scans agree. A file
    /// outside the repo base keeps the pre-namespace rule (only
    /// `__init__`-bearing ancestors).
    fn module_fqn_for(&self, file: &Path) -> String {
        let is_init = file.file_stem().is_some_and(|s| s == "__init__");
        let stem = file
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "module".to_string());
        // The identity boundary: walk up from the scan root for a `.git` entry
        // (the git toplevel), falling back to the scan root when no ancestor is
        // a checkout. Inline block — no new unit. The same repo-base model the
        // TS frontend uses (`ts.apg-tsfrontend.identity.packageIdentity`), so
        // the boundary is never the scan root.
        let repo_base = {
            let mut dir = self.root.clone();
            loop {
                if dir.join(".git").exists() {
                    break dir;
                }
                match dir.parent() {
                    Some(parent) => dir = parent.to_path_buf(),
                    None => break self.root.clone(),
                }
            }
        };
        let under_base = file.starts_with(&repo_base);
        let mut ancestors: Vec<String> = Vec::new();
        let mut dir = file.parent().map(Path::to_path_buf);
        while let Some(d) = dir {
            if under_base {
                // Directories strictly below the repo base all contribute;
                // the base itself is the boundary (exclusive).
                if d == repo_base {
                    break;
                }
            } else {
                // Outside the repo base: preserve the pre-namespace boundary.
                let has_init = d.join("__init__.py").is_file() || d.join("__init__.pyi").is_file();
                if !has_init {
                    break;
                }
            }
            if let Some(name) = d.file_name() {
                ancestors.push(name.to_string_lossy().into_owned());
            }
            dir = d.parent().map(Path::to_path_buf);
        }
        dotted_identity(is_init, &stem, &ancestors)
    }

    fn run(&mut self, out: &mut impl Write) {
        let files = self.discover_files();

        // ── ty project context (task-4). Markers only; no interpreter probe.
        let root_system = SystemPathBuf::from_path_buf_lossy(self.root.clone());
        let system = OsSystem::new(root_system.clone());
        let project_name = self
            .root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "root".to_string());
        let metadata = match ProjectMetadata::discover(&root_system, &system) {
            Ok(metadata) => metadata,
            Err(error) => {
                eprintln!(
                    "pyfrontend: project discovery fell back to a bare root ({error}); \
                     stdlib-only resolution"
                );
                ProjectMetadata::new(project_name, root_system.clone())
            }
        };
        let db =
            match ProjectDatabase::fallible(metadata.clone(), OsSystem::new(root_system.clone())) {
                Ok(db) => db,
                Err(error) => {
                    eprintln!(
                        "pyfrontend: project database fell back to defaults ({error:#}); \
                     stdlib-only resolution"
                    );
                    ProjectDatabase::use_defaults(metadata, OsSystem::new(root_system))
                }
            };

        // ── Discovery: identity + emit flag + ty File handle per file. The
        // File record is emitted for every accepted file even when ty cannot
        // build a handle, so an accepted file is never dropped.
        struct FileEntry {
            file: Option<File>,
            abs_path: String,
            identity: String,
            emit: bool,
        }
        let mut entries: Vec<FileEntry> = Vec::with_capacity(files.len());
        for path in &files {
            let identity = self.module_fqn_for(path);
            let emit = match &self.target_identities {
                None => true,
                Some(set) => set.contains(&identity),
            };
            // Module records are GLOBAL scaffolding (no location), emitted
            // verbatim for every discovered module regardless of the filter.
            self.emit_module_chain(&identity, out);
            let abs_path = path.to_string_lossy().into_owned();
            let sys_path = SystemPathBuf::from_path_buf_lossy(path.clone());
            let file = system_path_to_file(&db, &sys_path).ok();
            entries.push(FileEntry {
                file,
                abs_path,
                identity,
                emit,
            });
        }

        // ── File records (only the emitted set when a target filter is on).
        for entry in &entries {
            if !entry.emit {
                continue;
            }
            let line_count = std::fs::read_to_string(&entry.abs_path)
                .map(|text| text.lines().count().max(1) as u32)
                .unwrap_or(1);
            write_record(
                out,
                &serde_json::json!({
                    "type": "file",
                    "path": entry.abs_path,
                    "parent": entry.identity,
                    "start_line": 1,
                    "end_line": line_count,
                }),
            );
        }

        // ── Pass 1: collect declarations over the FULL context, emitting the
        // struct/function records only for the target set.
        for entry in &entries {
            let Some(file) = entry.file else { continue };
            let program_file = ProgramFile::new(&db, file, db.project().program(&db));
            let parsed = parsed_module(&db, program_file.python_file(&db)).load(&db);
            let li = line_index(&db, file);
            let source = ruff_db::source::source_text(&db, file);
            let src_str = source.as_str();
            let body = parsed.syntax().body.clone();
            self.walk_body(
                &body,
                &entry.identity,
                file,
                &entry.abs_path,
                &li,
                src_str,
                entry.emit,
                out,
            );
        }

        // Canonical FQN per declaration, computed over the FULL declaration
        // set (the full resolution context) so an edge to a declaration
        // outside the emission target set carries its canonical FQN instead of
        // a dangling opaque id. Overload grouping mirrors the ingestor's
        // `(parent, name)` rule.
        let mut groups: HashMap<(String, String), usize> = HashMap::new();
        for decl in &self.all_decls {
            if decl.kind == DeclKind::Function {
                *groups
                    .entry((decl.parent.clone(), decl.name.clone()))
                    .or_insert(0) += 1;
            }
        }
        let canonicals: Vec<String> = self
            .all_decls
            .iter()
            .map(|decl| {
                if decl.kind == DeclKind::Function {
                    let overloaded = groups
                        .get(&(decl.parent.clone(), decl.name.clone()))
                        .copied()
                        .unwrap_or(0)
                        > 1;
                    canonical_function_fqn(&decl.parent, &decl.name, &decl.params, overloaded)
                } else {
                    canonical_struct_fqn(&decl.parent, &decl.name)
                }
            })
            .collect();
        for (decl, canonical) in self.all_decls.iter_mut().zip(canonicals) {
            decl.canonical = canonical;
        }
        for decl in &self.all_decls {
            self.id_canonical
                .insert(decl.id.clone(), decl.canonical.clone());
            if decl.emit {
                self.emitted_id.insert(decl.id.clone());
            }
        }

        // ── Pass 2: exact call resolution via ty's call-hierarchy engine, for
        // every emitted function.
        let decls = std::mem::take(&mut self.all_decls);
        for decl in decls
            .iter()
            .filter(|d| d.kind == DeclKind::Function && d.emit)
        {
            let program_file = ProgramFile::new(&db, decl.file, db.project().program(&db));
            let calls = outgoing_calls(&db, program_file, decl.name_offset);
            for call in &calls {
                self.emit_call_edge(&db, &decl.id, &call.to, out);
            }
        }
        self.all_decls = decls;

        // ── Pass 3: base-class resolution (Uses edges) via goto-definition,
        // for every emitted file (classes in non-emitted files can never be
        // part of the emitted stream).
        for entry in &entries {
            if !entry.emit {
                continue;
            }
            let Some(file) = entry.file else { continue };
            let program_file = ProgramFile::new(&db, file, db.project().program(&db));
            let parsed = parsed_module(&db, program_file.python_file(&db)).load(&db);
            let body = parsed.syntax().body.clone();
            self.walk_bases(&db, &body, &entry.identity, file, out);
        }
    }

    /// Emits `module` records + a Module->Module `contains` chain for every
    /// dotted prefix of `identity` (deduplicated across files).
    fn emit_module_chain(&mut self, identity: &str, out: &mut impl Write) {
        let parts: Vec<&str> = identity.split('.').collect();
        let mut cur = String::new();
        for part in parts {
            let child = if cur.is_empty() {
                part.to_string()
            } else {
                format!("{cur}.{part}")
            };
            if self.seen_modules.insert(child.clone()) {
                write_record(out, &serde_json::json!({"type": "module", "fqn": child}));
            }
            if !cur.is_empty() {
                write_record(
                    out,
                    &serde_json::json!({"type": "contains", "from": cur, "to": child}),
                );
            }
            cur = child;
        }
    }

    /// Recursively walks a statement list, tracking the enclosing scope's
    /// FQN. Control-flow blocks (`if`/`for`/`while`/`with`/`try`/`match`)
    /// don't introduce a new Python scope, so their bodies are walked with
    /// the same `parent_fqn`; only `def`/`class` push a new scope. Declarations
    /// are always COLLECTED (the full resolution context) but only EMITTED
    /// when `emit` is true.
    #[allow(clippy::too_many_arguments)]
    fn walk_body(
        &mut self,
        body: &[Stmt],
        parent_fqn: &str,
        file: File,
        path: &str,
        li: &ruff_source_file::LineIndex,
        src: &str,
        emit: bool,
        out: &mut impl Write,
    ) {
        for stmt in body {
            match stmt {
                Stmt::FunctionDef(f) => {
                    let name = f.name.id.as_str();
                    let fqn = format!("{parent_fqn}.{name}");
                    let id = self.next_id();
                    let name_offset = f.name.range().start();
                    let (start, end) = (f.range().start(), f.range().end());
                    let start_line = li.line_index(start).get();
                    let end_line = li.line_index(end).get();
                    let params: Vec<String> = f
                        .parameters
                        .iter_non_variadic_params()
                        .filter(|p| !matches!(p.name().id.as_str(), "self" | "cls"))
                        .map(|p| {
                            p.annotation()
                                .map(|a| expr_source(a, src))
                                .unwrap_or_default()
                        })
                        .collect();
                    if emit {
                        write_record(
                            out,
                            &serde_json::json!({
                                "type": "function",
                                "id": id,
                                "parent": parent_fqn,
                                "name": name,
                                "params": params,
                                "file": path,
                                "path": path,
                                "start": u32::from(start),
                                "end": u32::from(end),
                                "start_line": start_line,
                                "end_line": end_line,
                            }),
                        );
                        if let Some(struct_id) = self.struct_id_by_fqn.get(parent_fqn).cloned() {
                            write_record(
                                out,
                                &serde_json::json!({
                                    "type": "contains",
                                    "from": self.endpoint(&struct_id),
                                    "to": id,
                                }),
                            );
                        }
                    }
                    self.decl_by_pos
                        .insert((file, u32::from(name_offset)), id.clone());
                    self.all_decls.push(Decl {
                        id,
                        kind: DeclKind::Function,
                        parent: parent_fqn.to_string(),
                        name: name.to_string(),
                        params,
                        canonical: String::new(),
                        file,
                        name_offset,
                        emit,
                    });
                    self.walk_body(&f.body, &fqn, file, path, li, src, emit, out);
                }
                Stmt::ClassDef(c) => {
                    let name = c.name.id.as_str();
                    let fqn = format!("{parent_fqn}.{name}");
                    let id = self.next_id();
                    let name_offset = c.name.range().start();
                    let (start, end) = (c.range().start(), c.range().end());
                    let start_line = li.line_index(start).get();
                    let end_line = li.line_index(end).get();
                    if emit {
                        write_record(
                            out,
                            &serde_json::json!({
                                "type": "struct",
                                "id": id,
                                "parent": parent_fqn,
                                "name": name,
                                "path": path,
                                "start": u32::from(start),
                                "end": u32::from(end),
                                "start_line": start_line,
                                "end_line": end_line,
                            }),
                        );
                        if let Some(struct_id) = self.struct_id_by_fqn.get(parent_fqn).cloned() {
                            write_record(
                                out,
                                &serde_json::json!({
                                    "type": "contains",
                                    "from": self.endpoint(&struct_id),
                                    "to": id,
                                }),
                            );
                        }
                    }
                    self.struct_id_by_fqn.insert(fqn.clone(), id.clone());
                    self.decl_by_pos
                        .insert((file, u32::from(name_offset)), id.clone());
                    self.all_decls.push(Decl {
                        id,
                        kind: DeclKind::Struct,
                        parent: parent_fqn.to_string(),
                        name: name.to_string(),
                        params: Vec::new(),
                        canonical: String::new(),
                        file,
                        name_offset,
                        emit,
                    });
                    self.walk_body(&c.body, &fqn, file, path, li, src, emit, out);
                }
                // Control-flow: same scope.
                Stmt::If(s) => {
                    self.walk_body(&s.body, parent_fqn, file, path, li, src, emit, out);
                    for clause in &s.elif_else_clauses {
                        self.walk_body(&clause.body, parent_fqn, file, path, li, src, emit, out);
                    }
                }
                Stmt::For(s) => {
                    self.walk_body(&s.body, parent_fqn, file, path, li, src, emit, out);
                    self.walk_body(&s.orelse, parent_fqn, file, path, li, src, emit, out);
                }
                Stmt::While(s) => {
                    self.walk_body(&s.body, parent_fqn, file, path, li, src, emit, out);
                    self.walk_body(&s.orelse, parent_fqn, file, path, li, src, emit, out);
                }
                Stmt::With(s) => {
                    self.walk_body(&s.body, parent_fqn, file, path, li, src, emit, out);
                }
                Stmt::Try(s) => {
                    self.walk_body(&s.body, parent_fqn, file, path, li, src, emit, out);
                    for handler in &s.handlers {
                        let ast::ExceptHandler::ExceptHandler(h) = handler;
                        self.walk_body(&h.body, parent_fqn, file, path, li, src, emit, out);
                    }
                    self.walk_body(&s.orelse, parent_fqn, file, path, li, src, emit, out);
                    self.walk_body(&s.finalbody, parent_fqn, file, path, li, src, emit, out);
                }
                Stmt::Match(s) => {
                    for case in &s.cases {
                        self.walk_body(&case.body, parent_fqn, file, path, li, src, emit, out);
                    }
                }
                _ => {}
            }
        }
    }

    /// Resolves each emitted class's base-class expressions to a `Uses` edge
    /// (or an `UnresolvedUse` when ty can't statically resolve it), walking
    /// the same scope structure as `walk_body` to recover each class's id.
    fn walk_bases(
        &mut self,
        db: &ProjectDatabase,
        body: &[Stmt],
        parent_fqn: &str,
        file: File,
        out: &mut impl Write,
    ) {
        for stmt in body {
            match stmt {
                Stmt::ClassDef(c) => {
                    let fqn = format!("{parent_fqn}.{}", c.name.id.as_str());
                    let class_id = self.struct_id_by_fqn.get(&fqn).cloned();
                    let emit_class = class_id
                        .as_ref()
                        .is_some_and(|id| self.emitted_id.contains(id));
                    if let (true, Some(class_id), Some(arguments)) =
                        (emit_class, class_id.clone(), c.arguments.as_deref())
                    {
                        for base in &arguments.args {
                            if let Some(name_offset) = base_identifier_offset(base) {
                                let program_file =
                                    ProgramFile::new(db, file, db.project().program(db));
                                if let Some(ranged) = goto_definition(db, program_file, name_offset)
                                {
                                    for target in &ranged.value {
                                        let key = (
                                            target.file(),
                                            u32::from(target.focus_range().start()),
                                        );
                                        let target_id = self.decl_by_pos.get(&key).cloned();
                                        if let Some(target_id) = target_id {
                                            write_record(
                                                out,
                                                &serde_json::json!({
                                                    "type": "uses",
                                                    "from": class_id,
                                                    "to": self.endpoint(&target_id),
                                                }),
                                            );
                                        } else {
                                            let unresolved_fqn =
                                                unresolved_name_for(db, target.file(), &self.root);
                                            let (origin, path) = file_origin(db, target.file());
                                            let category =
                                                classify_unresolved(origin, &path, &self.root);
                                            self.emit_unresolved(&unresolved_fqn, category, out);
                                            write_record(
                                                out,
                                                &serde_json::json!({
                                                    "type": "unresolved_use",
                                                    "from": class_id,
                                                    "to": unresolved_fqn,
                                                }),
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }
                    self.walk_bases(db, &c.body, &fqn, file, out);
                }
                Stmt::FunctionDef(f) => {
                    let fqn = format!("{parent_fqn}.{}", f.name.id.as_str());
                    self.walk_bases(db, &f.body, &fqn, file, out);
                }
                Stmt::If(s) => {
                    self.walk_bases(db, &s.body, parent_fqn, file, out);
                    for clause in &s.elif_else_clauses {
                        self.walk_bases(db, &clause.body, parent_fqn, file, out);
                    }
                }
                Stmt::For(s) => {
                    self.walk_bases(db, &s.body, parent_fqn, file, out);
                    self.walk_bases(db, &s.orelse, parent_fqn, file, out);
                }
                Stmt::While(s) => {
                    self.walk_bases(db, &s.body, parent_fqn, file, out);
                    self.walk_bases(db, &s.orelse, parent_fqn, file, out);
                }
                Stmt::With(s) => {
                    self.walk_bases(db, &s.body, parent_fqn, file, out);
                }
                Stmt::Try(s) => {
                    self.walk_bases(db, &s.body, parent_fqn, file, out);
                    for handler in &s.handlers {
                        let ast::ExceptHandler::ExceptHandler(h) = handler;
                        self.walk_bases(db, &h.body, parent_fqn, file, out);
                    }
                    self.walk_bases(db, &s.orelse, parent_fqn, file, out);
                    self.walk_bases(db, &s.finalbody, parent_fqn, file, out);
                }
                Stmt::Match(s) => {
                    for case in &s.cases {
                        self.walk_bases(db, &case.body, parent_fqn, file, out);
                    }
                }
                _ => {}
            }
        }
    }

    fn emit_call_edge(
        &mut self,
        db: &ProjectDatabase,
        from_id: &str,
        to: &CallHierarchyItem,
        out: &mut impl Write,
    ) {
        // A resolved call whose target is a class (constructor invocation,
        // `Dog()`) is a `Uses` edge (Function->Struct), matching the schema
        // every other frontend follows; only a resolved function/method target
        // is a `Calls` edge (Function->Function).
        let edge_kind = if to.kind == ty_ide::SymbolKind::Class {
            "uses"
        } else {
            "calls"
        };
        let key = (to.file, u32::from(to.selection_range.start()));
        if let Some(target_id) = self.decl_by_pos.get(&key).cloned() {
            write_record(
                out,
                &serde_json::json!({
                    "type": edge_kind,
                    "from": from_id,
                    "to": self.endpoint(&target_id),
                }),
            );
            return;
        }
        let unresolved_fqn = match &to.detail {
            Some(module) if !module.is_empty() => format!("{module}.{}", to.name),
            _ => to.name.to_string(),
        };
        let (origin, path) = file_origin(db, to.file);
        let category = classify_unresolved(origin, &path, &self.root);
        self.emit_unresolved(&unresolved_fqn, category, out);
        if edge_kind == "uses" {
            write_record(
                out,
                &serde_json::json!({
                    "type": "unresolved_use",
                    "from": from_id,
                    "to": unresolved_fqn,
                }),
            );
        } else {
            write_record(
                out,
                &serde_json::json!({
                    "type": "unresolved_call",
                    "from": from_id,
                    "to": unresolved_fqn,
                    "target_type": "",
                }),
            );
        }
    }

    fn emit_unresolved(&mut self, fqn: &str, category: &str, out: &mut impl Write) {
        if fqn.is_empty() || !self.seen_unresolved.insert(fqn.to_string()) {
            return;
        }
        write_record(
            out,
            &serde_json::json!({"type": "unresolved", "fqn": fqn, "category": category}),
        );
    }
}

/// The identifier offset naming a base-class expression (`Base` in
/// `class Foo(Base):`, or `Base` in `class Foo(module.Base):`), used as the
/// query point for `goto_definition`.
fn base_identifier_offset(expr: &ast::Expr) -> Option<TextSize> {
    match expr {
        ast::Expr::Name(n) => Some(n.range().start()),
        ast::Expr::Attribute(a) => Some(a.attr.range().start()),
        _ => None,
    }
}

/// Best-effort source snippet for a parameter annotation expression, used as
/// the erased "type" string in `params` (mirrors the other frontends' erased
/// parameter types, used ingestor-side only for overload-group
/// disambiguation).
fn expr_source(expr: &ast::Expr, src: &str) -> String {
    let range = expr.range();
    src.get(usize::from(range.start())..usize::from(range.end()))
        .unwrap_or("")
        .to_string()
}

/// The `ty` path origin plus a plain `Path`, for category classification.
fn file_origin(db: &ProjectDatabase, file: File) -> (PathOrigin, PathBuf) {
    match file.path(db) {
        FilePath::Vendored(p) => (PathOrigin::Vendored, PathBuf::from(p.as_str())),
        FilePath::System(p) => (PathOrigin::System, p.as_std_path().to_path_buf()),
        FilePath::SystemVirtual(p) => (PathOrigin::SystemVirtual, PathBuf::from(p.as_str())),
    }
}

/// Builds a stable "unresolved" FQN for a `ty`-resolved definition that falls
/// outside our own declaration set (stdlib, third-party, or otherwise outside
/// the project root).
fn unresolved_name_for(db: &ProjectDatabase, file: File, root: &Path) -> String {
    match file.path(db) {
        FilePath::Vendored(p) => p.as_str().to_string(),
        FilePath::System(p) => {
            let std_path = p.as_std_path();
            std_path
                .strip_prefix(root)
                .unwrap_or(std_path)
                .to_string_lossy()
                .into_owned()
        }
        FilePath::SystemVirtual(p) => p.as_str().to_string(),
    }
}

/// Reads the pinned `--targets` list (phase-02 task-9): absolute source paths,
/// one per line, blanks ignored. `None` — an absent flag, an unreadable file or
/// an empty list — means NO emission filter (the byte-identical full scan).
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
        .map(|l| {
            let p = PathBuf::from(l);
            std::fs::canonicalize(&p).unwrap_or(p)
        })
        .collect();
    if set.is_empty() { None } else { Some(set) }
}

/// The pinned per-language native-artifact location for the Python frontend,
/// `<cache-dir>/py/<cache-key>/` (phase-02 task-9 NATIVE-ARTIFACT RULE). ty
/// resolves through an in-process salsa database, so (exactly as the Rust
/// frontend does for rust-analyzer) the directory carries no separate on-disk
/// compiler cache; creating it makes the shared store's Python location exist
/// and keeps it keyed by the global cache key, so a key drift lands in a fresh
/// directory.
fn ensure_artifact_dir(cache_dir: Option<&str>, cache_key: Option<&str>) {
    let Some(dir) = cache_dir else { return };
    let mut p = PathBuf::from(dir);
    p.push("py");
    if let Some(k) = cache_key {
        p.push(k);
    }
    if let Err(e) = std::fs::create_dir_all(&p) {
        eprintln!(
            "warning: could not create py artifact dir {}: {e}",
            p.display()
        );
    }
}

fn write_record(out: &mut impl Write, value: &serde_json::Value) {
    let _ = serde_json::to_writer(&mut *out, value);
    let _ = out.write_all(b"\n");
}

pub struct Args {
    root: PathBuf,
    module_dirs: Vec<PathBuf>,
    excludes: Vec<String>,
    id_prefix: String,
    targets_path: Option<String>,
    cache_dir: Option<String>,
    cache_key: Option<String>,
}

fn usage() -> String {
    "usage: pyfrontend <dir> [--module <dir>]... [--id-prefix <p>] \
     [--targets <file>] [--cache-dir <dir>] [--cache-key <key>] [exclude...]"
        .to_string()
}

pub fn parse_args(argv: &[String]) -> Result<Args, String> {
    let Some(first) = argv.first() else {
        return Err(usage());
    };
    let mut args = Args {
        root: PathBuf::from(first),
        module_dirs: Vec::new(),
        excludes: Vec::new(),
        id_prefix: "n".to_string(),
        targets_path: None,
        cache_dir: None,
        cache_key: None,
    };
    let mut i = 1;
    while i < argv.len() {
        let next = argv.get(i + 1).cloned();
        match argv[i].as_str() {
            "--module" => match next {
                Some(v) => {
                    let p = if v == "." {
                        args.root.clone()
                    } else {
                        args.root.join(v)
                    };
                    args.module_dirs.push(p);
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
            "--cache-dir" => match next {
                Some(v) => {
                    args.cache_dir = Some(v);
                    i += 2;
                }
                None => {
                    args.excludes.push(argv[i].clone());
                    i += 1;
                }
            },
            "--cache-key" => match next {
                Some(v) => {
                    args.cache_key = Some(v);
                    i += 2;
                }
                None => {
                    args.excludes.push(argv[i].clone());
                    i += 1;
                }
            },
            // Rust-only flag threaded through by `apg scan` for every
            // frontend command line in a multi-language scan; harmless no-op
            // here.
            "--no-build-scripts" => i += 1,
            other => {
                args.excludes.push(other.to_string());
                i += 1;
            }
        }
    }
    Ok(args)
}

pub fn run(args: Args) -> io::Result<()> {
    let root = std::fs::canonicalize(&args.root).unwrap_or(args.root);
    let module_dirs: Vec<PathBuf> = args
        .module_dirs
        .iter()
        .map(|d| std::fs::canonicalize(d).unwrap_or_else(|_| d.clone()))
        .collect();
    let target_set = read_target_set(args.targets_path.as_deref());
    ensure_artifact_dir(args.cache_dir.as_deref(), args.cache_key.as_deref());

    let stdout = io::stdout();
    let mut out = io::BufWriter::new(stdout.lock());

    // Resolve the target paths to module identities with the same rule the
    // discovery walk uses (package/module granularity, task-7).
    let mut scanner = Scanner::new(
        root.clone(),
        module_dirs,
        args.excludes,
        args.id_prefix,
        None,
    );
    let target_identities = target_set.as_ref().map(|set| {
        let targets: Vec<PathBuf> = set.iter().cloned().collect();
        identities_from_targets(&targets, |p| scanner.module_fqn_for(p))
    });
    scanner.target_identities = target_identities;

    let kind = detect_project_kind(&root);
    let file_count = scanner.discover_files().len();
    eprintln!(
        "pyfrontend: {kind:?} project at {} ({file_count} Python source files)",
        root.display()
    );

    scanner.run(&mut out);
    out.flush()
}

#[cfg(test)]
mod tests;
