//! The Python scanner: declaration collection and the JSONL emission passes.

mod discovery;
mod emit;
mod identity;
mod walk;

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::PathBuf;

use ruff_db::files::{File, system_path_to_file};
use ruff_db::parsed::parsed_module;
use ruff_db::source::line_index;
use ruff_db::system::{OsSystem, SystemPathBuf};
use ruff_text_size::TextSize;
use ty_ide::outgoing_calls;
use ty_project::{Db as _, ProjectDatabase, ProjectMetadata};
use ty_python_core::ProgramFile;

use crate::identity::{canonical_function_fqn, canonical_struct_fqn};
use crate::jsonl::write_record;

/// One declared struct/function, with enough info to emit its record and to
/// later resolve edges pointing at it.
pub(crate) struct Decl {
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
pub(crate) enum DeclKind {
    Struct,
    Function,
}

pub(crate) struct Scanner {
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
    pub(crate) target_identities: Option<HashSet<String>>,
}

impl Scanner {
    pub(crate) fn new(
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

    pub(crate) fn next_id(&mut self) -> String {
        let id = format!("{}{}", self.id_prefix, self.next_id);
        self.next_id += 1;
        id
    }

    /// The endpoint to emit for an edge target: the opaque id when the
    /// declaration belongs to the emitted stream, else its canonical FQN (the
    /// cached-fact splice resolves that against the reused unit). With no
    /// filter in force every id is emitted, so this is always `id`.
    pub(crate) fn endpoint(&self, id: &str) -> String {
        if self.emitted_id.contains(id) {
            id.to_string()
        } else {
            self.id_canonical
                .get(id)
                .cloned()
                .unwrap_or_else(|| id.to_string())
        }
    }

    pub(crate) fn run(&mut self, out: &mut impl Write) {
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
}
