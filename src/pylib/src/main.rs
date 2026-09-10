// apg Python scanner frontend.
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
// Module model: a directory containing `__init__.py`/`__init__.pyi` is a
// package; the dotted FQN is built by walking up through `__init__`-bearing
// ancestors. A file with no `__init__.py` in its directory is a flat
// top-level module named by its stem (matching plain-script / namespace
// layouts). FQNs:
//
//   pkg/sub/__init__.py         -> module `pkg.sub`
//   pkg/sub/mod.py              -> module `pkg.sub.mod`
//   pkg/sub/mod.py class Foo    -> struct `pkg.sub.mod.Foo`
//   pkg/sub/mod.py Foo.method   -> function `pkg.sub.mod.Foo.method`
//
// start/end are 0-based UTF-8 byte offsets (Ruff's native `TextSize`);
// start_line/end_line are 1-based inclusive line numbers.
//
// Usage: pyfrontend <dir> [--module <dir>]... [--id-prefix <p>] [exclude...]
//   Common non-project directories (`__pycache__`, `.venv`, `venv`, `.tox`,
//   `.git`, `.mypy_cache`, `.pytest_cache`, `.ruff_cache`, `node_modules`) are
//   always skipped; `--module` restricts scanning to the given directories;
//   remaining args are substring path excludes.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use ruff_db::files::{system_path_to_file, File, FilePath};
use ruff_db::parsed::parsed_module;
use ruff_db::source::line_index;
use ruff_db::system::{OsSystem, SystemPathBuf};
use ruff_python_ast::{self as ast, Stmt};
use ruff_text_size::{Ranged, TextSize};
use ty_ide::{goto_definition, outgoing_calls, CallHierarchyItem};
use ty_project::{Db as _, ProjectDatabase, ProjectMetadata};
use ty_python_core::ProgramFile;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("Usage: pyfrontend <dir> [--module <dir>]... [--id-prefix <p>] [exclude...]");
        std::process::exit(1);
    }

    let root_arg = PathBuf::from(&args[0]);
    let root = std::fs::canonicalize(&root_arg).unwrap_or(root_arg);

    let mut module_dirs: Vec<PathBuf> = Vec::new();
    let mut excludes: Vec<String> = Vec::new();
    let mut id_prefix = "n".to_string();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--module" if i + 1 < args.len() => {
                let d = &args[i + 1];
                let p = if d == "." { root.clone() } else { root.join(d) };
                module_dirs.push(p);
                i += 2;
            }
            "--id-prefix" if i + 1 < args.len() => {
                id_prefix = args[i + 1].clone();
                i += 2;
            }
            // Rust-only flag threaded through by `apg scan` for every
            // frontend command line in a multi-language scan; harmless no-op
            // here.
            "--no-build-scripts" => {
                i += 1;
            }
            other => {
                excludes.push(other.to_string());
                i += 1;
            }
        }
    }

    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());

    let mut scanner = Scanner::new(root, module_dirs, excludes, id_prefix);
    scanner.run(&mut out);
}

/// One declared struct/function, with enough info to emit its record and to
/// later resolve edges pointing at it. `fqn`/`path` are kept for
/// debuggability even though the current passes only key off `id`/`file`/
/// `name_offset`/`kind`.
#[allow(dead_code)]
struct Decl {
    id: String,
    fqn: String,
    kind: DeclKind,
    file: File,
    path: String,
    /// Byte offset of the declaration's name identifier — the offset ty's
    /// call-hierarchy / goto-definition APIs use to identify + resolve it.
    name_offset: TextSize,
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
    /// back to the declaration we already emitted.
    decl_by_pos: HashMap<(File, u32), String>,
    all_decls: Vec<Decl>,
}

impl Scanner {
    fn new(
        root: PathBuf,
        module_dirs: Vec<PathBuf>,
        excludes: Vec<String>,
        id_prefix: String,
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
        }
    }

    fn next_id(&mut self) -> String {
        let id = format!("{}{}", self.id_prefix, self.next_id);
        self.next_id += 1;
        id
    }

    fn is_excluded_dir(name: &str) -> bool {
        matches!(
            name,
            "__pycache__"
                | ".venv"
                | "venv"
                | ".tox"
                | ".git"
                | ".hg"
                | ".mypy_cache"
                | ".pytest_cache"
                | ".ruff_cache"
                | "node_modules"
                | ".eggs"
        ) || name.ends_with(".egg-info")
    }

    fn is_path_excluded(&self, path: &Path) -> bool {
        let s = path.to_string_lossy();
        self.excludes.iter().any(|pat| s.contains(pat.as_str()))
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
            if Self::is_excluded_dir(&name) {
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

    /// Computes the dotted module FQN for a source file by walking up through
    /// `__init__.py`/`__init__.pyi`-bearing ancestor directories.
    fn module_fqn_for(&self, file: &Path) -> String {
        let is_init = file.file_stem().is_some_and(|s| s == "__init__");
        let mut parts: Vec<String> = Vec::new();
        if !is_init {
            if let Some(stem) = file.file_stem() {
                parts.push(stem.to_string_lossy().into_owned());
            }
        }
        let mut dir = file.parent().map(Path::to_path_buf);
        while let Some(d) = dir {
            let has_init = d.join("__init__.py").is_file() || d.join("__init__.pyi").is_file();
            if !has_init {
                break;
            }
            if let Some(name) = d.file_name() {
                parts.push(name.to_string_lossy().into_owned());
            }
            dir = d.parent().map(Path::to_path_buf);
        }
        parts.reverse();
        if parts.is_empty() {
            // Top-level file with no package name (e.g. a lone `main.py`):
            // fall back to the file stem even for `__init__.py` at the scan
            // root, so the module still has a non-empty name.
            file.file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "module".to_string())
        } else {
            parts.join(".")
        }
    }

    fn run(&mut self, out: &mut impl std::io::Write) {
        let files = self.discover_files();
        if files.is_empty() {
            return;
        }

        let root_system = SystemPathBuf::from_path_buf_lossy(self.root.clone());
        let system = OsSystem::new(root_system.clone());
        let metadata = ProjectMetadata::new(
            self.root
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "root".to_string()),
            root_system,
        );
        let db = ProjectDatabase::use_defaults(metadata, system);

        // Map every source path to (File, module fqn), emitting `module` and
        // `file` records eagerly, and Module->Module contains chains.
        let mut file_handles: Vec<(File, String, String)> = Vec::new(); // (File, abs path, module fqn)
        for path in &files {
            let sys_path = SystemPathBuf::from_path_buf_lossy(path.clone());
            let file = match system_path_to_file(&db, &sys_path) {
                Ok(f) => f,
                Err(_) => continue,
            };
            let module_fqn = self.module_fqn_for(path);
            self.emit_module_chain(module_fqn.as_str(), out);

            let text = std::fs::read_to_string(path).unwrap_or_default();
            let line_count = text.lines().count().max(1) as u32;
            let abs_path = path.to_string_lossy().into_owned();
            write_record(
                out,
                &serde_json::json!({
                    "type": "file",
                    "path": abs_path,
                    "parent": module_fqn,
                    "start_line": 1,
                    "end_line": line_count,
                }),
            );
            file_handles.push((file, abs_path, module_fqn));
        }

        // Pass 1: walk each file's AST, assign opaque ids, emit struct /
        // function declarations, and register (File, offset) -> id so pass 2
        // can map ty's resolved call/reference targets back to our ids.
        for (file, abs_path, module_fqn) in &file_handles {
            let program_file = ProgramFile::new(&db, *file, db.project().program(&db));
            let parsed = parsed_module(&db, program_file.python_file(&db)).load(&db);
            let li = line_index(&db, *file);
            let source = ruff_db::source::source_text(&db, *file);
            let src_str = source.as_str();
            let body = parsed.syntax().body.clone();
            self.walk_body(&body, module_fqn, *file, abs_path, &li, src_str, out);
        }

        // Pass 2: exact call resolution via ty's call-hierarchy engine.
        let decls = std::mem::take(&mut self.all_decls);
        for decl in decls.iter().filter(|d| d.kind == DeclKind::Function) {
            let program_file = ProgramFile::new(&db, decl.file, db.project().program(&db));
            let calls = outgoing_calls(&db, program_file, decl.name_offset);
            for call in &calls {
                self.emit_call_edge(&db, &decl.id, &call.to, out);
            }
        }

        // Pass 3: base-class resolution (Uses edges) via goto-definition.
        for (file, abs_path, module_fqn) in &file_handles {
            let program_file = ProgramFile::new(&db, *file, db.project().program(&db));
            let parsed = parsed_module(&db, program_file.python_file(&db)).load(&db);
            let body = parsed.syntax().body.clone();
            self.walk_bases(&db, &body, module_fqn, *file, abs_path, out);
        }
    }

    /// Emits `module` records + a Module->Module `contains` chain for every
    /// dotted prefix of `fqn` (deduplicated across files).
    fn emit_module_chain(&mut self, fqn: &str, out: &mut impl std::io::Write) {
        let parts: Vec<&str> = fqn.split('.').collect();
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
    /// the same `parent_fqn`; only `def`/`class` push a new scope.
    #[allow(clippy::too_many_arguments)]
    fn walk_body(
        &mut self,
        body: &[Stmt],
        parent_fqn: &str,
        file: File,
        path: &str,
        li: &ruff_source_file::LineIndex,
        src: &str,
        out: &mut impl std::io::Write,
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
                    if let Some(struct_id) = self.struct_id_by_fqn.get(parent_fqn) {
                        write_record(
                            out,
                            &serde_json::json!({"type": "contains", "from": struct_id, "to": id}),
                        );
                    }
                    self.decl_by_pos
                        .insert((file, u32::from(name_offset)), id.clone());
                    self.all_decls.push(Decl {
                        id,
                        fqn: fqn.clone(),
                        kind: DeclKind::Function,
                        file,
                        path: path.to_string(),
                        name_offset,
                    });
                    self.walk_body(&f.body, &fqn, file, path, li, src, out);
                }
                Stmt::ClassDef(c) => {
                    let name = c.name.id.as_str();
                    let fqn = format!("{parent_fqn}.{name}");
                    let id = self.next_id();
                    let name_offset = c.name.range().start();
                    let (start, end) = (c.range().start(), c.range().end());
                    let start_line = li.line_index(start).get();
                    let end_line = li.line_index(end).get();
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
                    if let Some(struct_id) = self.struct_id_by_fqn.get(parent_fqn) {
                        write_record(
                            out,
                            &serde_json::json!({"type": "contains", "from": struct_id, "to": id}),
                        );
                    }
                    self.struct_id_by_fqn.insert(fqn.clone(), id.clone());
                    self.decl_by_pos
                        .insert((file, u32::from(name_offset)), id.clone());
                    self.all_decls.push(Decl {
                        id,
                        fqn: fqn.clone(),
                        kind: DeclKind::Struct,
                        file,
                        path: path.to_string(),
                        name_offset,
                    });
                    self.walk_body(&c.body, &fqn, file, path, li, src, out);
                }
                // Control-flow: same scope.
                Stmt::If(s) => {
                    self.walk_body(&s.body, parent_fqn, file, path, li, src, out);
                    for clause in &s.elif_else_clauses {
                        self.walk_body(&clause.body, parent_fqn, file, path, li, src, out);
                    }
                }
                Stmt::For(s) => {
                    self.walk_body(&s.body, parent_fqn, file, path, li, src, out);
                    self.walk_body(&s.orelse, parent_fqn, file, path, li, src, out);
                }
                Stmt::While(s) => {
                    self.walk_body(&s.body, parent_fqn, file, path, li, src, out);
                    self.walk_body(&s.orelse, parent_fqn, file, path, li, src, out);
                }
                Stmt::With(s) => {
                    self.walk_body(&s.body, parent_fqn, file, path, li, src, out);
                }
                Stmt::Try(s) => {
                    self.walk_body(&s.body, parent_fqn, file, path, li, src, out);
                    for handler in &s.handlers {
                        let ast::ExceptHandler::ExceptHandler(h) = handler;
                        self.walk_body(&h.body, parent_fqn, file, path, li, src, out);
                    }
                    self.walk_body(&s.orelse, parent_fqn, file, path, li, src, out);
                    self.walk_body(&s.finalbody, parent_fqn, file, path, li, src, out);
                }
                Stmt::Match(s) => {
                    for case in &s.cases {
                        self.walk_body(&case.body, parent_fqn, file, path, li, src, out);
                    }
                }
                _ => {}
            }
        }
    }

    /// Resolves each class's base-class expressions to a `Uses` edge (or an
    /// `UnresolvedUse` when ty can't statically resolve it), walking the same
    /// scope structure as `walk_body` to recover each class's opaque id.
    fn walk_bases(
        &mut self,
        db: &ProjectDatabase,
        body: &[Stmt],
        parent_fqn: &str,
        file: File,
        path: &str,
        out: &mut impl std::io::Write,
    ) {
        for stmt in body {
            match stmt {
                Stmt::ClassDef(c) => {
                    let fqn = format!("{parent_fqn}.{}", c.name.id.as_str());
                    if let (Some(class_id), Some(arguments)) = (
                        self.struct_id_by_fqn.get(&fqn).cloned(),
                        c.arguments.as_deref(),
                    ) {
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
                                        if let Some(target_id) = self.decl_by_pos.get(&key) {
                                            write_record(
                                                out,
                                                &serde_json::json!({"type": "uses", "from": class_id, "to": target_id}),
                                            );
                                        } else {
                                            let unresolved_fqn =
                                                unresolved_name_for(db, target.file(), &self.root);
                                            let category =
                                                unresolved_category(db, target.file(), &self.root);
                                            self.emit_unresolved(&unresolved_fqn, &category, out);
                                            write_record(
                                                out,
                                                &serde_json::json!({"type": "unresolved_use", "from": class_id, "to": unresolved_fqn}),
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }
                    self.walk_bases(db, &c.body, &fqn, file, path, out);
                }
                Stmt::FunctionDef(f) => {
                    let fqn = format!("{parent_fqn}.{}", f.name.id.as_str());
                    self.walk_bases(db, &f.body, &fqn, file, path, out);
                }
                Stmt::If(s) => {
                    self.walk_bases(db, &s.body, parent_fqn, file, path, out);
                    for clause in &s.elif_else_clauses {
                        self.walk_bases(db, &clause.body, parent_fqn, file, path, out);
                    }
                }
                Stmt::For(s) => {
                    self.walk_bases(db, &s.body, parent_fqn, file, path, out);
                    self.walk_bases(db, &s.orelse, parent_fqn, file, path, out);
                }
                Stmt::While(s) => {
                    self.walk_bases(db, &s.body, parent_fqn, file, path, out);
                    self.walk_bases(db, &s.orelse, parent_fqn, file, path, out);
                }
                Stmt::With(s) => {
                    self.walk_bases(db, &s.body, parent_fqn, file, path, out);
                }
                Stmt::Try(s) => {
                    self.walk_bases(db, &s.body, parent_fqn, file, path, out);
                    for handler in &s.handlers {
                        let ast::ExceptHandler::ExceptHandler(h) = handler;
                        self.walk_bases(db, &h.body, parent_fqn, file, path, out);
                    }
                    self.walk_bases(db, &s.orelse, parent_fqn, file, path, out);
                    self.walk_bases(db, &s.finalbody, parent_fqn, file, path, out);
                }
                Stmt::Match(s) => {
                    for case in &s.cases {
                        self.walk_bases(db, &case.body, parent_fqn, file, path, out);
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
        out: &mut impl std::io::Write,
    ) {
        // A resolved call whose target is a class (constructor invocation,
        // `Dog()`) is a `Uses` edge (Function->Struct), matching the schema
        // every other frontend follows (Go routes type conversions the same
        // way); only a resolved function/method target is a `Calls` edge
        // (Function->Function).
        let edge_kind = if to.kind == ty_ide::SymbolKind::Class {
            "uses"
        } else {
            "calls"
        };
        let key = (to.file, u32::from(to.selection_range.start()));
        if let Some(target_id) = self.decl_by_pos.get(&key) {
            write_record(
                out,
                &serde_json::json!({"type": edge_kind, "from": from_id, "to": target_id}),
            );
        } else {
            let unresolved_fqn = match &to.detail {
                Some(module) if !module.is_empty() => format!("{module}.{}", to.name),
                _ => to.name.to_string(),
            };
            let category = unresolved_category(db, to.file, &self.root);
            self.emit_unresolved(&unresolved_fqn, &category, out);
            let record_type = if edge_kind == "uses" {
                "unresolved_use"
            } else {
                "unresolved_call"
            };
            if record_type == "unresolved_call" {
                write_record(
                    out,
                    &serde_json::json!({
                        "type": record_type,
                        "from": from_id,
                        "to": unresolved_fqn,
                        "target_type": "",
                    }),
                );
            } else {
                write_record(
                    out,
                    &serde_json::json!({
                        "type": record_type,
                        "from": from_id,
                        "to": unresolved_fqn,
                    }),
                );
            }
        }
    }

    fn emit_unresolved(&mut self, fqn: &str, category: &str, out: &mut impl std::io::Write) {
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
/// parameter types, used ingestor-side only for overload-group disambiguation).
fn expr_source(expr: &ast::Expr, src: &str) -> String {
    let range = expr.range();
    src.get(usize::from(range.start())..usize::from(range.end()))
        .unwrap_or("")
        .to_string()
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

fn unresolved_category(db: &ProjectDatabase, file: File, root: &Path) -> String {
    match file.path(db) {
        FilePath::Vendored(_) => "stdlib".to_string(),
        FilePath::System(p) => {
            if p.as_std_path().starts_with(root) {
                // Inside the project but not one of our own declarations
                // (e.g. a dynamically-created attribute) — closest category
                // is "unknown" rather than fabricating a resolved edge.
                "unknown".to_string()
            } else {
                "external".to_string()
            }
        }
        FilePath::SystemVirtual(_) => "unknown".to_string(),
    }
}

fn write_record(out: &mut impl std::io::Write, value: &serde_json::Value) {
    let _ = serde_json::to_writer(&mut *out, value);
    let _ = out.write_all(b"\n");
}
