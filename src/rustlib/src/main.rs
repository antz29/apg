//! apg Rust scanner frontend. Parses a Cargo workspace with rust-analyzer (the
//! exact-fidelity resolver stack) and streams the unified JSONL schema (SPEC §2)
//! to stdout. Exact tier: calls and types resolve through `hir::Semantics`;
//! anything that is not a project symbol becomes an `unresolved_call` /
//! `unresolved_use` edge with a category, never a fabricated FQN.
//!
//! Method parenting: impl methods (inherent and trait) hang under their **self
//! type**; trait declarations and default methods hang under the **trait**.
//! `resolve_method_call` resolves a call on a concrete receiver to the impl
//! block's function, so internal calls land on the exact declared node, and
//! same-trait/different-type impls render distinct FQNs instead of colliding on
//! the trait.

use std::collections::{HashMap, HashSet};
use std::io::Write;

use anyhow::Result;
use hir::{
    Adt, AssocItem, Crate, Function, HasSource, Impl, InFile, Module, ModuleDef, PathResolution,
    Semantics, Trait,
};
use ide_db::base_db::{CrateOrigin, SourceDatabase};
use ide_db::FxHashMap;
use ide_db::RootDatabase;
use load_cargo::{load_workspace, LoadCargoConfig, ProcMacroServerChoice};
use project_model::{
    CargoConfig, CargoFeatures, CargoWorkspace, ProjectManifest, ProjectWorkspace,
    ProjectWorkspaceKind, RustLibSource,
};
use syntax::ast::{self, AstNode};
use vfs::{AbsPathBuf, FileId, Vfs};

// ── Unified schema records (SPEC §2) ──────────────────────────────────

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
    Function {
        id: String,
        parent: String,
        name: String,
        params: Vec<String>,
        file: String,
        path: String,
        start: u32,
        end: u32,
        start_line: u32,
        end_line: u32,
    },
    Unresolved {
        fqn: String,
        category: Option<String>,
    },
    Contains {
        from: String,
        to: String,
    },
    Calls {
        from: String,
        to: String,
    },
    Uses {
        from: String,
        to: String,
    },
    UnresolvedCall {
        from: String,
        to: String,
        #[serde(default)]
        target_type: String,
    },
    UnresolvedUse {
        from: String,
        to: String,
    },
}

#[derive(Clone)]
struct Decl {
    kind: &'static str,
    id: String,
    parent: String,
    name: String,
    params: Vec<String>,
    path: String,
    file: String,
    start: u32,
    end: u32,
    start_line: u32,
    end_line: u32,
    src_key: (String, u32),
    /// Whether this declaration belongs to a crate selected for emission
    /// (phase-05 task-8). With no `--targets` filter every declaration is
    /// emitted, so the stream is byte-identical to a full scan.
    emit: bool,
}

struct ImplEdge {
    self_fqn: String,
    trait_fqn: String,
}

struct State {
    next_id: usize,
    /// Canonical FQN (`parent.name`) -> id, for all struct-like nodes.
    struct_id: HashMap<String, String>,
    /// (path, byte offset) -> id, for every declared struct and function.
    id_by_source: HashMap<(String, u32), String>,
    /// (path, byte offset) -> id, for every declared struct-like node.
    struct_sources: HashMap<(String, u32), String>,
    /// id -> the canonical FQN the ingestor renders for it (including the
    /// `parent.name(params)` overload suffix). Built over the FULL collected
    /// declaration set — the full resolution context — so a cross-crate edge to
    /// a declaration outside the emission target set can carry its canonical
    /// FQN instead of a dangling opaque id (phase-05 task-8).
    id_fqn: HashMap<String, String>,
    /// The ids whose node records are actually part of the emitted stream (the
    /// target set, or every declaration when no filter is in force). An edge to
    /// an id NOT in this set carries the target's canonical FQN, which the
    /// ingestor's cached-fact splice resolves against the reused unit.
    emitted_id: HashSet<String>,
    /// Dedup of unresolved records by fqn (first category wins).
    unresolved_seen: HashSet<String>,
    impl_edges: Vec<ImplEdge>,
}

impl State {
    fn new() -> State {
        State {
            next_id: 0,
            struct_id: HashMap::new(),
            id_by_source: HashMap::new(),
            struct_sources: HashMap::new(),
            id_fqn: HashMap::new(),
            emitted_id: HashSet::new(),
            unresolved_seen: HashSet::new(),
            impl_edges: Vec::new(),
        }
    }

    /// The endpoint to emit for an edge target with opaque `id`: the id when the
    /// declaration is part of the emitted stream, else the declaration's
    /// canonical FQN. With no filter in force every id is emitted, so this is
    /// always `id` — byte-identical to a full scan.
    fn endpoint(&self, id: &str) -> String {
        if self.emitted_id.contains(id) {
            id.to_string()
        } else {
            self.id_fqn
                .get(id)
                .cloned()
                .unwrap_or_else(|| id.to_string())
        }
    }
}

/// Immutable view of the resolution maps used by the pass-2 syntax walk. Bundled
/// so handlers can resolve an edge target to the opaque id (or its canonical
/// FQN when the target is outside the emission target set) while the walk still
/// mutates `unresolved_seen` independently.
struct Endpoints<'a> {
    id_by_source: &'a HashMap<(String, u32), String>,
    struct_sources: &'a HashMap<(String, u32), String>,
    id_fqn: &'a HashMap<String, String>,
    emitted_id: &'a HashSet<String>,
}

impl Endpoints<'_> {
    fn endpoint(&self, id: &str) -> String {
        if self.emitted_id.contains(id) {
            id.to_string()
        } else {
            self.id_fqn
                .get(id)
                .cloned()
                .unwrap_or_else(|| id.to_string())
        }
    }

    /// Endpoint for a function/struct source key, when it resolves.
    fn fn_id(&self, key: &(String, u32)) -> Option<String> {
        self.id_by_source.get(key).map(|id| self.endpoint(id))
    }

    /// Endpoint for a struct-like source key, when it resolves.
    fn struct_id(&self, key: &(String, u32)) -> Option<String> {
        self.struct_sources.get(key).map(|id| self.endpoint(id))
    }
}

struct Ctx<'db> {
    db: &'db RootDatabase,
    sema: Semantics<'db, RootDatabase>,
    vfs: &'db Vfs,
    /// Cargo package name keyed by the crate-root file's absolute path, for
    /// every target of every package the loaded project resolved (phase-05
    /// task-10). Consulted by [`package_prefix_map`] to group local crates by
    /// package and to prefer the package identity over the target/display name.
    package_by_root: HashMap<String, String>,
    /// Resolved prefix override by crate-root absolute path: the cargo package
    /// name for a single-target package, and the distinct `<pkg>-bin` prefix for
    /// the bin of a colliding lib + bin package (libbin-fix). Populated per
    /// loaded project by [`scan`]; an absent entry means "use the display-name
    /// fallback".
    package_prefix: HashMap<String, String>,
}

// ── CLI ───────────────────────────────────────────────────────────────

fn main() {
    if let Err(e) = run(std::env::args().collect()) {
        eprintln!("rustfrontend: {e:#}");
        std::process::exit(1);
    }
}

fn run(args: Vec<String>) -> Result<()> {
    if args.len() < 2 {
        eprintln!(
            "Usage: rustfrontend <dir> [--module <dir>]... [--no-build-scripts] \
             [--targets <file>] [--cache-dir <dir>] [--cache-key <key>] [exclude...]"
        );
        std::process::exit(1);
    }
    let root = args[1].clone();
    let mut module_dirs: Vec<String> = Vec::new();
    let mut excludes: Vec<String> = Vec::new();
    let mut no_build_scripts = false;
    // id prefix (`--id-prefix`, default "n") keeps opaque ids unique across
    // frontends when a scan merges multiple languages.
    let mut id_prefix = "n";
    // The pinned phase-02 target-set hand-off (task-9): `--targets` is an
    // emission filter, `--cache-dir`/`--cache-key` locate the shared
    // per-language native-artifact directory.
    let mut targets_path: Option<String> = None;
    let mut cache_dir: Option<String> = None;
    let mut cache_key: Option<String> = None;
    {
        let mut i = 2;
        while i < args.len() {
            let a = args[i].as_str();
            if a == "--module" && i + 1 < args.len() {
                module_dirs.push(args[i + 1].clone());
                i += 1;
            } else if a == "--no-build-scripts" {
                no_build_scripts = true;
            } else if a == "--id-prefix" && i + 1 < args.len() {
                id_prefix = &args[i + 1];
                i += 1;
            } else if a == "--targets" && i + 1 < args.len() {
                targets_path = Some(args[i + 1].clone());
                i += 1;
            } else if a == "--cache-dir" && i + 1 < args.len() {
                cache_dir = Some(args[i + 1].clone());
                i += 1;
            } else if a == "--cache-key" && i + 1 < args.len() {
                cache_key = Some(args[i + 1].clone());
                i += 1;
            } else {
                excludes.push(args[i].clone());
            }
            i += 1;
        }
    }
    let root_abs = std::path::Path::new(&root);
    let root_abs = root_abs
        .canonicalize()
        .unwrap_or_else(|_| root_abs.to_path_buf());
    let module_dirs: Vec<String> = module_dirs
        .iter()
        .map(|d| {
            let p = std::path::Path::new(d);
            let abs = if p.is_absolute() {
                p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
            } else {
                root_abs
                    .join(p)
                    .canonicalize()
                    .unwrap_or_else(|_| root_abs.join(p))
            };
            abs.display().to_string()
        })
        .collect();

    // The target set is an EMISSION filter only: every discovered project is
    // still fully loaded and resolved (global.constraint.frontend-full-context).
    let target_filter = read_target_set(targets_path.as_deref());
    ensure_artifact_dir(cache_dir.as_deref(), cache_key.as_deref());

    // ── Discover and load every Cargo project under the scan root ──────
    // Each discovered workspace root is loaded once; its members are resolved
    // through it and never re-loaded as top-level projects (phase-05 task-2).
    // The nested `src/rustlib` crate is a standalone project, not a member of
    // the root workspace, and stays byte-identical on disk — discovery never
    // mutates a manifest (phase-05 task-4).
    let manifest_dirs = discover_manifest_dirs(&root_abs);
    if manifest_dirs.is_empty() {
        eprintln!(
            "Error: no Rust workspace found under {}",
            root_abs.display()
        );
        std::process::exit(1);
    }

    let out = std::io::stdout();
    let mut w = std::io::BufWriter::new(out.lock());
    let mut state = State::new();
    let mut loaded_roots: Vec<std::path::PathBuf> = Vec::new();
    let mut total_crates = 0usize;
    for dir in &manifest_dirs {
        // Dedupe by manifest root: a candidate directory a previously loaded
        // workspace already covers (its crate lives under it) is skipped, so a
        // workspace member is never re-loaded as a top-level project and no
        // module node is double-counted.
        if loaded_roots.iter().any(|r| r.starts_with(dir)) {
            continue;
        }
        let (db, vfs, package_by_root) = match load_project(dir, no_build_scripts) {
            Ok(x) => x,
            Err(e) => {
                eprintln!("warning: skipping project {}: {e:#}", dir.display());
                continue;
            }
        };
        let roots = hir::attach_db(&db, || local_crate_root_files(&db, &vfs));
        loaded_roots.extend(roots);
        let sema = Semantics::new(&db);
        let ctx = Ctx {
            db: &db,
            sema,
            vfs: &vfs,
            package_by_root,
            package_prefix: HashMap::new(),
        };
        // Type inference (method resolution, type resolution) interns through a
        // thread-local db; run each project's scan inside it. The opaque-id
        // counter and resolution maps are shared across projects so ids stay
        // unique in the merged stream.
        total_crates += hir::attach_db(ctx.db, || {
            scan(
                ctx,
                &root_abs,
                module_dirs.clone(),
                excludes.clone(),
                id_prefix,
                target_filter.as_ref(),
                &mut state,
                &mut w,
            )
        })?;
    }
    let _ = w.flush();
    if total_crates == 0 {
        eprintln!(
            "Error: no Rust workspace found under {}",
            root_abs.display()
        );
        std::process::exit(1);
    }
    Ok(())
}

/// Emits facts for one loaded project into the shared stream, returning the
/// number of local crates scanned. `state` is shared across projects so opaque
/// ids remain unique across the merged multi-project stream; the project's
/// `db`/`vfs` stay alive only for the duration of the call.
#[allow(clippy::too_many_arguments)]
fn scan(
    mut ctx: Ctx<'_>,
    root_abs: &std::path::Path,
    module_dirs: Vec<String>,
    excludes: Vec<String>,
    id_prefix: &str,
    target_filter: Option<&HashSet<std::path::PathBuf>>,
    state: &mut State,
    w: &mut impl Write,
) -> Result<usize> {
    // ── Pass 1a: module records + Module→Module containment, per crate. ──
    let mut crates: Vec<Crate> = Crate::all(ctx.db)
        .into_iter()
        .filter(|k| k.origin(ctx.db).is_local())
        .filter(|k| within_module_limit(&ctx, *k, &module_dirs))
        .collect();
    // Prefer the cargo PACKAGE name as each crate's module prefix (phase-05
    // task-10): `src/rustlib` declares `[package] name = "apg-rustfrontend"`
    // but `[[bin]] name = "rustfrontend"`, so the target/display name would
    // otherwise render `rustfrontend.*` and shadow the package identity. The
    // package name is preferred ONLY when the package maps to exactly one local
    // crate. A package with several targets keeps each target's display name,
    // EXCEPT when two would render the same prefix (the default lib + bin
    // package): the bin then gets a distinct `<pkg>-bin` prefix so distinct
    // crates never collapse onto one module FQN (libbin-fix).
    ctx.package_prefix = package_prefix_map(&ctx, &crates);
    crates.sort_by_key(|k| crate_prefix(&ctx, *k));

    if crates.is_empty() {
        return Ok(0);
    }

    let mut targeted: HashSet<Crate> = HashSet::new();
    let mut module_fqn: HashMap<Module, String> = HashMap::new();
    let mut module_nodes: Vec<String> = Vec::new();
    let mut module_edges: Vec<(String, String)> = Vec::new();
    for krate in &crates {
        let prefix = crate_prefix(&ctx, *krate);
        let root_mod = krate.root_module(ctx.db);
        let mut seen: HashSet<Module> = HashSet::new();
        let mut stack: Vec<Module> = vec![root_mod];
        // A crate is re-emitted when any of its source files is in the target
        // set (per-crate/module granularity, phase-05 task-8). No filter in
        // force selects every crate.
        let mut crate_emit = target_filter.is_none();
        while let Some(m) = stack.pop() {
            if !seen.insert(m) {
                continue;
            }
            if !crate_emit {
                if let Some(ed) = m.as_source_file_id(ctx.db) {
                    let path = path_of(&ctx, ed.file_id(ctx.db));
                    if target_filter.is_some_and(|t| t.contains(std::path::Path::new(&path))) {
                        crate_emit = true;
                    }
                }
            }
            let fqn = module_fqn_with_prefix(&ctx, m, &prefix);
            module_fqn.insert(m, fqn.clone());
            module_nodes.push(fqn.clone());
            let mut children: Vec<Module> = m.children(ctx.db).collect();
            children.sort_by_key(|c| module_fqn_with_prefix(&ctx, *c, &prefix));
            for child in &children {
                let cf = module_fqn_with_prefix(&ctx, *child, &prefix);
                module_edges.push((fqn.clone(), cf));
            }
            stack.extend(children);
        }
        if crate_emit {
            targeted.insert(*krate);
        }
    }
    // Module records are GLOBAL scaffolding, emitted for every loaded crate
    // regardless of the emission filter: they carry no location and so are not
    // part of any per-file fact unit, and a full scan's module set plus
    // Module→Module hierarchy must be present verbatim for the incremental
    // graph to equal a full scan. Only the per-file facts are filtered.
    module_nodes.sort();
    module_edges.sort();
    for fqn in &module_nodes {
        rec(w, Rec::Module { fqn: fqn.clone() });
    }
    for (from, to) in &module_edges {
        rec(
            w,
            Rec::Contains {
                from: from.clone(),
                to: to.clone(),
            },
        );
    }

    // ── Pass 1b: collect declarations over the FULL context. ──
    let mut decls: Vec<Decl> = Vec::new();
    let mut file_module: HashMap<FileId, String> = HashMap::new();
    let mut file_emit: HashMap<FileId, bool> = HashMap::new();
    for krate in &crates {
        let crate_emit = targeted.contains(krate);
        let prefix = crate_prefix(&ctx, *krate);
        let root_mod = krate.root_module(ctx.db);
        let mut seen: HashSet<Module> = HashSet::new();
        let mut stack: Vec<Module> = vec![root_mod];
        while let Some(m) = stack.pop() {
            if !seen.insert(m) {
                continue;
            }
            let mod_fqn = module_fqn_with_prefix(&ctx, m, &prefix);
            module_fqn.insert(m, mod_fqn.clone());

            // The file that backs this module (crate root or `mod foo;`).
            if let Some(ed) = m.as_source_file_id(ctx.db) {
                let fid = ed.file_id(ctx.db);
                file_module.entry(fid).or_insert(mod_fqn.clone());
                file_emit.entry(fid).or_insert(crate_emit);
            }

            for def in m.declarations(ctx.db) {
                match def {
                    ModuleDef::Function(f) => {
                        if let Some(d) = fn_decl(&ctx, f, &mod_fqn) {
                            push_decl(state, &mut decls, d, crate_emit);
                        }
                    }
                    ModuleDef::Adt(adt) => {
                        if let Some(d) = adt_decl(&ctx, adt, &mod_fqn) {
                            push_decl(state, &mut decls, d, crate_emit);
                        }
                        if let Adt::Enum(e) = adt {
                            // Enum variants hang under the enum.
                            let enum_fqn = format!("{}.{}", mod_fqn, e.name(ctx.db).as_str());
                            for v in e.variants(ctx.db) {
                                if let Some(d) = variant_decl(&ctx, v, &enum_fqn) {
                                    push_decl(state, &mut decls, d, crate_emit);
                                }
                            }
                        }
                    }
                    ModuleDef::Trait(t) => {
                        if let Some(d) = trait_decl(&ctx, t, &mod_fqn) {
                            push_decl(state, &mut decls, d, crate_emit);
                        }
                        // Trait methods (declarations and defaults) hang under
                        // the trait, like Go interface methods under a type.
                        let trait_fqn = format!("{}.{}", mod_fqn, t.name(ctx.db).as_str());
                        for item in t.items(ctx.db) {
                            if let AssocItem::Function(f) = item {
                                if let Some(d) = fn_decl(&ctx, f, &trait_fqn) {
                                    push_decl(state, &mut decls, d, crate_emit);
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            for imp in m.impl_defs(ctx.db) {
                process_impl(&ctx, imp, &mod_fqn, state, &mut decls, crate_emit);
            }
            stack.extend(m.children(ctx.db));
        }
    }
    let crate_count = crates.len();
    drop(crates);

    decls.sort_by_key(|d| (d.path.clone(), d.start));

    // Canonical FQN per declaration exactly as the ingestor renders it
    // (SPEC §4): `parent.name`, or `parent.name(params)` for a function whose
    // `(parent, name)` group is overloaded. Computed over the FULL collected
    // declaration set — the full resolution context — so an edge to a
    // declaration outside the emission target set can carry its canonical FQN
    // instead of a dangling opaque id (phase-05 task-8).
    let mut fn_groups: HashMap<(String, String), usize> = HashMap::new();
    for d in &decls {
        if d.kind == "function" {
            *fn_groups
                .entry((d.parent.clone(), d.name.clone()))
                .or_insert(0) += 1;
        }
    }

    // Assign opaque ids in sorted emission order (deterministic across runs),
    // and register the id maps used by pass 2 and structural edges. The id maps
    // cover every collected declaration (the full context); `emitted_id` marks
    // the subset whose node records are actually emitted.
    for d in &mut decls {
        state.next_id += 1;
        d.id = format!("{}{}", id_prefix, state.next_id);
        let overloaded = d.kind == "function"
            && fn_groups
                .get(&(d.parent.clone(), d.name.clone()))
                .copied()
                .unwrap_or(0)
                > 1;
        let fqn = if overloaded {
            format!("{}.{}({})", d.parent, d.name, d.params.join(","))
        } else {
            format!("{}.{}", d.parent, d.name)
        };
        state.id_fqn.insert(d.id.clone(), fqn.clone());
        // Generated/out-dir declarations are never code nodes (phase-05 task-3
        // PART 2), so they are withheld from the emitted set even when their
        // crate is targeted.
        if d.emit && under_excluded_tree(&d.path, root_abs) {
            d.emit = false;
        }
        if d.emit {
            state.emitted_id.insert(d.id.clone());
        }
        let key = d.src_key.clone();
        if d.kind == "struct" {
            state.struct_id.entry(fqn).or_insert_with(|| d.id.clone());
            state
                .struct_sources
                .entry(key.clone())
                .or_insert_with(|| d.id.clone());
        }
        state
            .id_by_source
            .entry(key)
            .or_insert_with(|| d.id.clone());
    }

    // ── Emission: file records, node records, structural edges, impl edges. ──
    let mut files: Vec<FileId> = file_module.keys().copied().collect();
    files.sort_by_key(|f| path_of(&ctx, *f));
    // A file is emitted when its crate is targeted and its path is neither
    // user-excluded nor a generated/dependency tree (task-3 PART 2 / task-8).
    let emitted_file = |fid: &FileId| -> bool {
        file_emit.get(fid).copied().unwrap_or(false)
            && !path_excluded(&path_of(&ctx, *fid), &excludes)
            && !under_excluded_tree(&path_of(&ctx, *fid), root_abs)
    };
    let total_files = files.iter().filter(|f| emitted_file(f)).count();
    let mut scanned = 0usize;
    for fid in &files {
        if !emitted_file(fid) {
            continue;
        }
        let path = path_of(&ctx, *fid);
        let text = ctx.db.file_text(*fid).text(ctx.db).to_string();
        let parent = file_module.get(fid).cloned().unwrap_or_default();
        rec(
            w,
            Rec::File {
                path: path.clone(),
                parent,
                start_line: 1,
                end_line: line_count(&text),
            },
        );
        scanned += 1;
        eprintln!(
            "\rScanning: {}% ({}/{})",
            scanned * 100 / total_files.max(1),
            scanned,
            total_files
        );
    }

    for d in &decls {
        if !d.emit {
            continue;
        }
        let _ = writeln!(w, "{}", node_record(d));
    }

    // Structural containment: a unit whose parent is a struct-like node hangs
    // under it (methods under self type, trait methods under trait, enum
    // variants under enum).
    for d in &decls {
        if !d.emit {
            continue;
        }
        if let Some(pid) = state.struct_id.get(&d.parent).cloned() {
            rec(
                w,
                Rec::Contains {
                    from: state.endpoint(&pid),
                    to: state.endpoint(&d.id),
                },
            );
        }
    }

    // `impl Trait for Self`: Uses to a project trait, UnresolvedUse to a
    // foreign one. Only when the self type is a project struct that is part of
    // the emitted stream.
    let impl_edges = std::mem::take(&mut state.impl_edges);
    for ie in &impl_edges {
        let Some(from) = state.struct_id.get(&ie.self_fqn).cloned() else {
            continue;
        };
        if !state.emitted_id.contains(&from) {
            continue;
        }
        if let Some(to) = state.struct_id.get(&ie.trait_fqn).cloned() {
            rec(
                w,
                Rec::Uses {
                    from: state.endpoint(&from),
                    to: state.endpoint(&to),
                },
            );
        } else {
            let from_ep = state.endpoint(&from);
            emit_unresolved(state, w, &ie.trait_fqn, "external");
            rec(
                w,
                Rec::UnresolvedUse {
                    from: from_ep,
                    to: ie.trait_fqn.clone(),
                },
            );
        }
    }

    // ── Pass 2: edge records from a syntax walk of every emitted file. ──
    {
        let eps = Endpoints {
            id_by_source: &state.id_by_source,
            struct_sources: &state.struct_sources,
            id_fqn: &state.id_fqn,
            emitted_id: &state.emitted_id,
        };
        for fid in &files {
            if !emitted_file(fid) {
                continue;
            }
            let _ = ctx.db.file_text(*fid).text(ctx.db);
            walk_file(&ctx, *fid, &eps, &mut state.unresolved_seen, w);
        }
    }
    let _ = w.flush();
    Ok(crate_count)
}

fn push_decl(state: &mut State, decls: &mut Vec<Decl>, mut d: Decl, emit: bool) {
    // Ids are assigned later, in sorted (path, start) order, for deterministic
    // output. Until then the decl carries an empty id.
    d.emit = emit;
    decls.push(d);
    let _ = state;
}

fn node_record(d: &Decl) -> String {
    let rec: Rec = match d.kind {
        "struct" => Rec::Struct {
            id: d.id.clone(),
            parent: d.parent.clone(),
            name: d.name.clone(),
            path: d.path.clone(),
            start: d.start,
            end: d.end,
            start_line: d.start_line,
            end_line: d.end_line,
        },
        _ => Rec::Function {
            id: d.id.clone(),
            parent: d.parent.clone(),
            name: d.name.clone(),
            params: d.params.clone(),
            file: d.file.clone(),
            path: d.path.clone(),
            start: d.start,
            end: d.end,
            start_line: d.start_line,
            end_line: d.end_line,
        },
    };
    serde_json::to_string(&rec).unwrap()
}

// ── workspace discovery ───────────────────────────────────────────────

/// Directory names never descended into by the all-manifest discovery walk
/// (phase-05 task-3 PART 1): a `Cargo.toml` inside any of them is never a scan
/// root. Matched on a whole path component, so a file named `target.rs` or a
/// directory named `targets` is unaffected.
const DISCOVERY_EXCLUDED_DIRS: &[&str] = &["target", "vendor", "node_modules", ".worktrees"];

fn is_discovery_excluded_dir(name: &str) -> bool {
    DISCOVERY_EXCLUDED_DIRS.contains(&name)
}

/// True when `path` carries a discovery-excluded directory component BELOW
/// `root`. The exclusion is relative to the scan root, so a project checked out
/// under a `.worktrees/` directory (the apg project flow) is not itself
/// excluded, while a `target/`, `vendor/`, `node_modules/` or nested
/// `.worktrees/` tree inside it is (phase-05 task-3 PART 2).
fn under_excluded_tree(path: &str, root: &std::path::Path) -> bool {
    let p = std::path::Path::new(path);
    let rel = p.strip_prefix(root).unwrap_or(p);
    rel.components().any(|c| match c {
        std::path::Component::Normal(s) => s.to_str().is_some_and(is_discovery_excluded_dir),
        _ => false,
    })
}

/// Every directory at or below `root` that holds a `Cargo.toml`, with the
/// generated/dependency/other-tree directories pruned (phase-05 task-1/-3).
/// Nested non-workspace crates are found; members of a discovered workspace are
/// found too and deduped by the load loop in [`run`].
fn discover_manifest_dirs(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out: Vec<std::path::PathBuf> = Vec::new();
    let mut stack: Vec<std::path::PathBuf> = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if dir.join("Cargo.toml").is_file() {
            out.push(dir.clone());
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(ft) = entry.file_type() else { continue };
            if !ft.is_dir() {
                continue;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            // `.git` is pruned too — it can never hold a manifest and walking it
            // is pure overhead; it is not part of the task's exclusion set.
            if is_discovery_excluded_dir(&name) || name == ".git" {
                continue;
            }
            stack.push(entry.path());
        }
    }
    // Shallow-first: a workspace root is loaded before its members, so the
    // members are then covered (and skipped) by the workspace that loaded them.
    out.sort_by(|a, b| {
        a.components()
            .count()
            .cmp(&b.components().count())
            .then_with(|| a.cmp(b))
    });
    out.dedup();
    out
}

/// The root files of every local crate the loaded workspace resolved. Used to
/// dedupe discovered projects by manifest root: a candidate directory a
/// previously loaded workspace already covers is never re-loaded as a top-level
/// project (phase-05 task-2).
fn local_crate_root_files(db: &RootDatabase, vfs: &Vfs) -> Vec<std::path::PathBuf> {
    Crate::all(db)
        .into_iter()
        .filter(|k| k.origin(db).is_local())
        .map(|k| path_of_vfs(vfs, k.root_file(db)))
        .filter(|p| !p.is_empty())
        .map(std::path::PathBuf::from)
        .collect()
}

/// The absolute target set read from the pinned `--targets <file>` hand-off
/// (phase-02 task-9). An absent flag, a missing/unreadable file, or an empty
/// file yields `None` — "no emission filter", the byte-identical full scan.
fn read_target_set(path: Option<&str>) -> Option<HashSet<std::path::PathBuf>> {
    let path = path?;
    let Ok(text) = std::fs::read_to_string(path) else {
        eprintln!("warning: could not read targets {path}; scanning unfiltered");
        return None;
    };
    let set: HashSet<std::path::PathBuf> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| {
            let p = std::path::PathBuf::from(l);
            // The VFS exposes canonical paths; normalize the target list the
            // same way so a symlinked scan root still matches.
            std::fs::canonicalize(&p).unwrap_or(p)
        })
        .collect();
    if set.is_empty() {
        None
    } else {
        Some(set)
    }
}

/// The pinned per-language native-artifact location for the Rust frontend,
/// `<cache-dir>/rust/<cache-key>/` (phase-02 task-9 NATIVE-ARTIFACT RULE).
/// rust-analyzer resolves through an in-process salsa database, so the
/// directory carries no separate on-disk compiler cache; creating it makes the
/// shared store's Rust location exist and keeps it keyed by the global cache
/// key, so a key drift lands in a fresh directory.
fn ensure_artifact_dir(cache_dir: Option<&str>, cache_key: Option<&str>) {
    let Some(dir) = cache_dir else { return };
    let mut p = std::path::PathBuf::from(dir);
    p.push("rust");
    if let Some(k) = cache_key {
        p.push(k);
    }
    if let Err(e) = std::fs::create_dir_all(&p) {
        eprintln!(
            "warning: could not create rust artifact dir {}: {e}",
            p.display()
        );
    }
}

fn load_project(
    root: &std::path::Path,
    no_build_scripts: bool,
) -> Result<(RootDatabase, Vfs, HashMap<String, String>)> {
    let progress = |_msg: String| {};
    let abs = AbsPathBuf::assert_utf8(root.to_path_buf());
    let manifest = ProjectManifest::discover_single(&abs)?;

    let mut cargo_config = CargoConfig {
        sysroot: Some(RustLibSource::Discover),
        all_targets: true,
        set_test: true,
        features: CargoFeatures::All,
        ..Default::default()
    };

    let load_config = LoadCargoConfig {
        load_out_dirs_from_check: !no_build_scripts,
        with_proc_macro_server: if no_build_scripts {
            ProcMacroServerChoice::None
        } else {
            ProcMacroServerChoice::Sysroot
        },
        prefill_caches: false,
        num_worker_threads: 1,
        proc_macro_processes: 1,
    };

    let mut ws = match ProjectWorkspace::load(manifest, &cargo_config, &progress) {
        Ok(ws) => ws,
        Err(e) => {
            eprintln!("warning: workspace load failed ({e}); retrying without sysroot");
            cargo_config.sysroot = None;
            let manifest = ProjectManifest::discover_single(&abs)?;
            ProjectWorkspace::load(manifest, &cargo_config, &progress)?
        }
    };

    let ws = if no_build_scripts {
        ws
    } else {
        match ws.run_build_scripts(&cargo_config, &progress) {
            Ok(bs) => {
                if let Some(err) = bs.error() {
                    eprintln!("warning: build scripts had errors: {err}");
                }
                ws.set_build_scripts(bs);
                ws
            }
            Err(e) => {
                eprintln!("warning: build scripts failed ({e}); continuing source-only");
                ws
            }
        }
    };

    // The cargo PACKAGE identity per crate-root file, retained from the loaded
    // project model before it is consumed by `load_workspace` (phase-05
    // task-10). The target/display name rust-analyzer derives can differ from
    // the package name (`[[bin]] name` vs `[package] name`), and the module
    // prefix must carry the package identity.
    let package_by_root = match &ws.kind {
        ProjectWorkspaceKind::Cargo { cargo, .. } => package_roots(cargo),
        _ => HashMap::new(),
    };

    let extra_env: FxHashMap<String, Option<String>> = FxHashMap::default();
    let (db, vfs, _) = load_workspace(ws, &extra_env, &load_config)?;
    Ok((db, vfs, package_by_root))
}

/// Cargo package name by the crate-root file's absolute path, for every target
/// of every package the loaded workspace resolved (phase-05 task-10). The map
/// also covers dependency packages; only the local crates' roots are ever
/// consulted (see [`package_prefix_map`]).
fn package_roots(cargo: &CargoWorkspace) -> HashMap<String, String> {
    let mut out: HashMap<String, String> = HashMap::new();
    for pkg in cargo.packages() {
        let data = &cargo[pkg];
        for target in &data.targets {
            out.insert(cargo[*target].root.to_string(), data.name.clone());
        }
    }
    out
}

/// One local crate's module-prefix inputs: its crate-root path, its cargo
/// package name, and the prefix it renders when no override applies.
struct LocalCrate {
    root: String,
    package: String,
    fallback: String,
}

/// The module-prefix override for one loaded project: crate-root path -> module
/// prefix, for the crates [`crate_prefix`] must not render from its fallback.
///
/// Two cases get an override:
///
/// * a package with EXACTLY ONE local crate target — the cargo package name is
///   preferred over the target/display name (`src/rustlib` declares
///   `[package] name = "apg-rustfrontend"` but `[[bin]] name = "rustfrontend"`,
///   so the display name would shadow the package identity) (phase-05 task-10);
/// * a package whose local crates would otherwise COLLIDE on the same fallback
///   prefix — the default lib + bin package, where both targets carry the
///   package name: the bin gets a distinct `<pkg>-bin` prefix so the two crate
///   roots render distinct module FQNs instead of the ingestor panicking on a
///   duplicate.
///
/// A multi-target package with DISTINCT target names gets no override and keeps
/// exactly its current `rust.foo` / `rust.foo-cli` rendering
/// (`requirements.constraint.rust-crate-fqn-stability`).
fn package_prefix_map(ctx: &Ctx<'_>, crates: &[Crate]) -> HashMap<String, String> {
    let locals: Vec<LocalCrate> = crates
        .iter()
        .filter_map(|k| {
            let root = path_of(ctx, k.root_file(ctx.db));
            let package = ctx.package_by_root.get(&root)?.clone();
            Some(LocalCrate {
                root,
                package,
                fallback: fallback_prefix(ctx, *k),
            })
        })
        .collect();

    let mut counts: HashMap<&str, usize> = HashMap::new();
    for l in &locals {
        *counts.entry(l.package.as_str()).or_insert(0) += 1;
    }

    // Local crates grouped by (package, fallback prefix). A group of more than
    // one is a display-name collision: without an override both crate roots
    // render the same module FQN and the ingestor panics on the duplicate.
    let mut groups: HashMap<(&str, &str), Vec<usize>> = HashMap::new();
    for (i, l) in locals.iter().enumerate() {
        groups
            .entry((l.package.as_str(), l.fallback.as_str()))
            .or_default()
            .push(i);
    }

    let mut prefix: HashMap<String, String> = HashMap::new();
    for (i, l) in locals.iter().enumerate() {
        let group = groups
            .get(&(l.package.as_str(), l.fallback.as_str()))
            .expect("every local crate was grouped above");
        if group.len() < 2 {
            // No collision: the single-target package takes the package-name
            // override; a multi-target package keeps the display-name fallback.
            if counts.get(l.package.as_str()).copied() == Some(1) {
                prefix.insert(l.root.clone(), l.package.clone());
            }
            continue;
        }
        // Collision: the keeper keeps its existing prefix and the bin(s) get a
        // distinct suffix. The keeper is the lib target (root file `lib.rs`)
        // when there is one, else the lowest root path — deterministic, and for
        // the default lib + bin package always the lib.
        let keeper = group
            .iter()
            .copied()
            .find(|&j| is_lib_root(&locals[j].root))
            .unwrap_or_else(|| *group.iter().min().expect("group is non-empty"));
        if i == keeper {
            continue;
        }
        // `<pkg>-bin` for the single colliding bin. Cargo admits at most a lib
        // and a bin sharing a name, so an over-full group is theoretical; a
        // further member is disambiguated by its root file stem.
        let others = group.iter().filter(|&&j| j != keeper).count();
        let suffix = if others <= 1 {
            "bin".to_string()
        } else {
            format!("bin-{}", root_stem(&l.root))
        };
        prefix.insert(l.root.clone(), format!("{}-{suffix}", l.package));
    }
    prefix
}

/// The prefix a crate renders when no override applies: its display name, then
/// its root-module name, then the literal `crate`.
fn fallback_prefix(ctx: &Ctx<'_>, krate: Crate) -> String {
    if let Some(display) = krate.display_name(ctx.db) {
        return display.to_string();
    }
    if let Some(name) = krate.root_module(ctx.db).name(ctx.db) {
        return name.as_str().to_string();
    }
    "crate".to_string()
}

/// Whether a crate-root path is a Cargo library target's root (`src/lib.rs`, or
/// a `[lib] path` ending in `lib.rs`).
fn is_lib_root(root: &str) -> bool {
    std::path::Path::new(root)
        .file_name()
        .is_some_and(|n| n == "lib.rs")
}

/// A crate-root file's stem, for disambiguating an over-full collision group.
fn root_stem(root: &str) -> String {
    std::path::Path::new(root)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| root.to_string())
}

// ── FQN / path helpers ────────────────────────────────────────────────

fn crate_prefix(ctx: &Ctx<'_>, krate: Crate) -> String {
    // The loaded project's override takes precedence (phase-05 task-10,
    // libbin-fix): the cargo PACKAGE name for a single-target package, and the
    // distinct `<pkg>-bin` prefix for the bin of a colliding lib + bin package.
    // Everything else renders its per-target display-name-then-root-module-name
    // fallback unchanged, so synthetic/no-manifest crates, distinct-name
    // multi-target packages, and foreign crates are unaffected.
    let root = path_of(ctx, krate.root_file(ctx.db));
    if let Some(pkg) = ctx.package_prefix.get(&root) {
        return pkg.clone();
    }
    fallback_prefix(ctx, krate)
}

fn module_fqn_full(ctx: &Ctx<'_>, m: Module) -> String {
    let prefix = crate_prefix(ctx, m.krate(ctx.db));
    module_fqn_with_prefix(ctx, m, &prefix)
}

fn module_fqn_with_prefix(ctx: &Ctx<'_>, m: Module, prefix: &str) -> String {
    let segments: Vec<String> = m
        .path_segments(ctx.db)
        .map(|n| n.as_str().to_string())
        .collect();
    if segments.is_empty() {
        prefix.to_string()
    } else {
        format!("{prefix}.{}", segments.join("."))
    }
}

fn within_module_limit(ctx: &Ctx<'_>, krate: Crate, module_dirs: &[String]) -> bool {
    if module_dirs.is_empty() {
        return true;
    }
    let path = path_of(ctx, krate.root_file(ctx.db));
    module_dirs.iter().any(|d| path.starts_with(d.as_str()))
}

fn path_of(ctx: &Ctx<'_>, file_id: FileId) -> String {
    path_of_vfs(ctx.vfs, file_id)
}

fn path_of_vfs(vfs: &Vfs, file_id: FileId) -> String {
    vfs.file_path(file_id)
        .as_path()
        .map(|p| p.to_string())
        .unwrap_or_default()
}

fn path_excluded(path: &str, excludes: &[String]) -> bool {
    excludes.iter().any(|p| path.contains(p.as_str()))
}

// ── declaration building ──────────────────────────────────────────────

fn source_key<A: AstNode>(ctx: &Ctx<'_>, f: InFile<A>) -> Option<(String, u32)> {
    let hir::HirFileId::FileId(real) = f.file_id else {
        return None;
    };
    let fid = real.file_id(ctx.db);
    let path = path_of(ctx, fid);
    if path.is_empty() {
        return None;
    }
    Some((path, u32::from(f.value.syntax().text_range().start())))
}

/// A located item's span: `(path, start, end, start_line, end_line, src_key)`.
type ItemLoc = (String, u32, u32, u32, u32, (String, u32));

/// (path, start, end, start_line, end_line, src_key) from a real-file item's
/// span. Macro-generated items return `None` and are skipped.
fn item_loc<A: AstNode>(ctx: &Ctx<'_>, f: InFile<A>) -> Option<ItemLoc> {
    let hir::HirFileId::FileId(real) = f.file_id else {
        return None;
    };
    let fid = real.file_id(ctx.db);
    let path = path_of(ctx, fid);
    if path.is_empty() {
        return None;
    }
    let range = f.value.syntax().text_range();
    let text = ctx.db.file_text(fid).text(ctx.db).to_string();
    let li = LineIndex::new(&text);
    let start = u32::from(range.start());
    let end = u32::from(range.end());
    let sl = li.line(start);
    let el = li.line(end.saturating_sub(1));
    let key = (path.clone(), start);
    Some((path, start, end, sl, el, key))
}

fn fn_decl(ctx: &Ctx<'_>, f: Function, parent: &str) -> Option<Decl> {
    let src = f.source(ctx.db)?;
    let params = fn_params(&src.value);
    let (path, start, end, sl, el, key) = item_loc(ctx, src)?;
    Some(Decl {
        kind: "function",
        id: String::new(),
        parent: parent.to_string(),
        name: f.name(ctx.db).as_str().to_string(),
        params,
        path: path.clone(),
        file: path.clone(),
        start,
        end,
        start_line: sl,
        end_line: el,
        src_key: key,
        emit: false,
    })
}

fn adt_decl(ctx: &Ctx<'_>, adt: Adt, parent: &str) -> Option<Decl> {
    let src = adt.source(ctx.db)?;
    let (path, start, end, sl, el, key) = item_loc(ctx, src)?;
    Some(Decl {
        kind: "struct",
        id: String::new(),
        parent: parent.to_string(),
        name: adt.name(ctx.db).as_str().to_string(),
        params: vec![],
        path: path.clone(),
        file: String::new(),
        start,
        end,
        start_line: sl,
        end_line: el,
        src_key: key,
        emit: false,
    })
}

fn variant_decl(ctx: &Ctx<'_>, v: hir::EnumVariant, parent: &str) -> Option<Decl> {
    let src = v.source(ctx.db)?;
    let (path, start, end, sl, el, key) = item_loc(ctx, src)?;
    Some(Decl {
        kind: "struct",
        id: String::new(),
        parent: parent.to_string(),
        name: v.name(ctx.db).as_str().to_string(),
        params: vec![],
        path: path.clone(),
        file: String::new(),
        start,
        end,
        start_line: sl,
        end_line: el,
        src_key: key,
        emit: false,
    })
}

fn trait_decl(ctx: &Ctx<'_>, t: Trait, parent: &str) -> Option<Decl> {
    let src = t.source(ctx.db)?;
    let (path, start, end, sl, el, key) = item_loc(ctx, src)?;
    Some(Decl {
        kind: "struct",
        id: String::new(),
        parent: parent.to_string(),
        name: t.name(ctx.db).as_str().to_string(),
        params: vec![],
        path: path.clone(),
        file: String::new(),
        start,
        end,
        start_line: sl,
        end_line: el,
        src_key: key,
        emit: false,
    })
}

fn fn_params(f: &ast::Fn) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(pl) = f.param_list() {
        for p in pl.params() {
            if let Some(t) = p.ty() {
                out.push(t.syntax().text().to_string().trim().to_string());
            } else {
                out.push(String::new());
            }
        }
    }
    out
}

fn self_type_fqn(ctx: &Ctx<'_>, imp: Impl) -> Option<String> {
    let ty = imp.self_ty(ctx.db);
    let adt = ty.autoderef(ctx.db).find_map(|t| t.as_adt())?;
    adt_self_fqn(ctx, adt)
}

/// `moduleFQN.name` for an ADT, project or foreign.
fn adt_self_fqn(ctx: &Ctx<'_>, adt: Adt) -> Option<String> {
    Some(format!(
        "{}.{}",
        module_fqn_full(ctx, adt.module(ctx.db)),
        adt.name(ctx.db).as_str()
    ))
}

fn process_impl(
    ctx: &Ctx<'_>,
    imp: Impl,
    mod_fqn: &str,
    state: &mut State,
    decls: &mut Vec<Decl>,
    crate_emit: bool,
) {
    // Builtin derive impls are macro-generated — skip (the source items are
    // the declarations).
    if imp.source(ctx.db).is_none() {
        return;
    }
    let self_fqn = self_type_fqn(ctx, imp);
    let parent = self_fqn.clone().unwrap_or_else(|| mod_fqn.to_string());

    if let Some(tr) = imp.trait_(ctx.db) {
        // `impl Trait for SelfType` — emitted once all struct ids are assigned
        // (project trait -> Uses; foreign -> UnresolvedUse), gated by the self
        // type being a project struct.
        let trait_fqn = format!(
            "{}.{}",
            module_fqn_full(ctx, tr.module(ctx.db)),
            tr.name(ctx.db).as_str()
        );
        if !parent.is_empty() && !trait_fqn.is_empty() {
            state.impl_edges.push(ImplEdge {
                self_fqn: parent.clone(),
                trait_fqn,
            });
        }
    }

    for item in imp.items(ctx.db) {
        if let AssocItem::Function(f) = item {
            if let Some(d) = fn_decl(ctx, f, &parent) {
                push_decl(state, decls, d, crate_emit);
            }
        }
    }
}

// ── emission helpers ──────────────────────────────────────────────────

fn rec<W: Write>(w: &mut W, r: Rec) {
    let _ = writeln!(w, "{}", serde_json::to_string(&r).unwrap());
}

fn emit_unresolved<W: Write>(state: &mut State, w: &mut W, fqn: &str, category: &str) {
    if fqn.is_empty() || !state.unresolved_seen.insert(fqn.to_string()) {
        return;
    }
    let _ = writeln!(
        w,
        "{}",
        serde_json::to_string(&Rec::Unresolved {
            fqn: fqn.to_string(),
            category: Some(category.to_string()),
        })
        .unwrap()
    );
}

fn crate_category(ctx: &Ctx<'_>, krate: Crate) -> &'static str {
    match krate.origin(ctx.db) {
        CrateOrigin::Lang(_) => "stdlib",
        CrateOrigin::Library { .. } => "external",
        _ => "unknown",
    }
}

/// How an unresolvable path is classified without resolution: std-liked
/// namespace prefixes are stdlib, qualified names external, bare names unknown.
fn path_category(path: &str) -> &'static str {
    if path.starts_with("std::") || path.starts_with("core::") || path.starts_with("alloc::") {
        "stdlib"
    } else if path.contains("::") {
        "external"
    } else {
        "unknown"
    }
}

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

struct LineIndex {
    starts: Vec<u32>,
}

impl LineIndex {
    fn new(text: &str) -> LineIndex {
        let mut starts = vec![0u32];
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                starts.push(i as u32 + 1);
            }
        }
        LineIndex { starts }
    }
    fn line(&self, offset: u32) -> u32 {
        match self.starts.binary_search(&offset) {
            Ok(l) => l as u32 + 1,
            Err(l) => l.max(1) as u32,
        }
    }
}

// ── pass 2: per-file syntax walk for edges ────────────────────────────

fn walk_file(
    ctx: &Ctx<'_>,
    file_id: FileId,
    eps: &Endpoints<'_>,
    unresolved: &mut HashSet<String>,
    w: &mut impl Write,
) {
    let sf = ctx.sema.parse_guess_edition(file_id);
    let node = sf.syntax().clone();
    walk_node(ctx, &node, None, eps, unresolved, w);
}

fn walk_node(
    ctx: &Ctx<'_>,
    node: &syntax::SyntaxNode,
    cur: Option<String>,
    eps: &Endpoints<'_>,
    unresolved: &mut HashSet<String>,
    w: &mut impl Write,
) {
    // Recompute the source unit when entering a fn / struct / enum / union /
    // trait. A nested fn that is not a module item (a closure or a local fn)
    // keeps the nearest planted ancestor as the edge source.
    if let Some(f) = ast::Fn::cast(node.clone()) {
        let new_cur = ctx
            .sema
            .to_fn_def(&f)
            .and_then(|def| def.source(ctx.db))
            .and_then(|src| source_key(ctx, src))
            .and_then(|k| eps.fn_id(&k))
            .or(cur);
        for child in node.children() {
            walk_node(ctx, &child, new_cur.clone(), eps, unresolved, w);
        }
        return;
    }
    if let Some(c) = current_item_id_for(ctx, eps, node.clone()) {
        for child in node.children() {
            walk_node(ctx, &child, Some(c.clone()), eps, unresolved, w);
        }
        return;
    }

    // Edge extraction for nodes directly in the current context.
    if let Some(c) = cur.as_ref() {
        if let Some(call) = ast::CallExpr::cast(node.clone()) {
            handle_call(ctx, &call, c, eps, unresolved, w);
        } else if let Some(mc) = ast::MethodCallExpr::cast(node.clone()) {
            handle_method_call(ctx, &mc, c, eps, unresolved, w);
        } else if let Some(mac) = ast::MacroCall::cast(node.clone()) {
            handle_macro(ctx, &mac, c, unresolved, w);
        } else if let Some(ty) = ast::Type::cast(node.clone()) {
            handle_type(ctx, &ty, c, eps, unresolved, w);
        } else if let Some(re) = ast::RecordExpr::cast(node.clone()) {
            handle_record(ctx, &re, c, eps, unresolved, w);
        }
    }
    for child in node.children() {
        walk_node(ctx, &child, cur.clone(), eps, unresolved, w);
    }
}

/// If `node` is a struct/enum/union/trait whose id is a project struct node,
/// returns that id (so field types and trait bounds attribute their edges to
/// the struct itself).
fn current_item_id_for(
    ctx: &Ctx<'_>,
    eps: &Endpoints<'_>,
    node: syntax::SyntaxNode,
) -> Option<String> {
    let adt = if let Some(s) = ast::Struct::cast(node.clone()) {
        ctx.sema.to_struct_def(&s).map(Adt::Struct)
    } else if let Some(s) = ast::Enum::cast(node.clone()) {
        ctx.sema.to_enum_def(&s).map(Adt::Enum)
    } else if let Some(s) = ast::Union::cast(node.clone()) {
        ctx.sema.to_union_def(&s).map(Adt::Union)
    } else {
        let t = ast::Trait::cast(node.clone())?;
        let d = ctx.sema.to_trait_def(&t)?;
        return source_key(ctx, d.source(ctx.db)?).and_then(|k| eps.struct_id(&k));
    };
    let d = adt?;
    let src = d.source(ctx.db)?;
    source_key(ctx, src).and_then(|k| eps.struct_id(&k))
}

// ── edge handlers ─────────────────────────────────────────────────────

fn handle_call(
    ctx: &Ctx<'_>,
    call: &ast::CallExpr,
    source: &str,
    eps: &Endpoints<'_>,
    unresolved: &mut HashSet<String>,
    w: &mut impl Write,
) {
    let Some(callee) = call.expr() else { return };
    match callee {
        ast::Expr::PathExpr(pe) => {
            let Some(path) = pe.path() else { return };
            match ctx.sema.resolve_path(&path) {
                Some(PathResolution::Def(ModuleDef::Function(f))) => {
                    emit_call(ctx, f, source, eps, unresolved, w)
                }
                Some(PathResolution::Def(ModuleDef::Adt(adt))) => {
                    emit_type_use(ctx, adt, source, eps, unresolved, w)
                }
                Some(PathResolution::Def(ModuleDef::EnumVariant(v))) => {
                    if let Some(key) = v
                        .source(ctx.db)
                        .and_then(|src| source_key(ctx, src))
                        .and_then(|k| eps.fn_id(&k))
                    {
                        edge(
                            w,
                            Rec::Uses {
                                from: source.to_string(),
                                to: key,
                            },
                        );
                    }
                }
                Some(PathResolution::Local(_)) => emit_unresolved_call(
                    ctx,
                    source,
                    &path.syntax().text().to_string(),
                    "func-value",
                    unresolved,
                    w,
                ),
                Some(_) => emit_unresolved_call(
                    ctx,
                    source,
                    &path.syntax().text().to_string(),
                    "unknown",
                    unresolved,
                    w,
                ),
                None => {
                    let name = path.syntax().text().to_string();
                    let cat = path_category(&name);
                    emit_unresolved_call(ctx, source, &name, cat, unresolved, w)
                }
            }
        }
        ast::Expr::ClosureExpr(_) => {
            emit_unresolved_call(ctx, source, "func", "func-value", unresolved, w)
        }
        _ => {
            let name = callee.syntax().text().to_string();
            let name = if name.len() > 64 {
                format!("{}…", &name[..64])
            } else {
                name
            };
            emit_unresolved_call(ctx, source, &name, "func-value", unresolved, w)
        }
    }
}

fn handle_method_call(
    ctx: &Ctx<'_>,
    call: &ast::MethodCallExpr,
    source: &str,
    eps: &Endpoints<'_>,
    unresolved: &mut HashSet<String>,
    w: &mut impl Write,
) {
    match ctx.sema.resolve_method_call(call) {
        Some(f) => emit_call(ctx, f, source, eps, unresolved, w),
        None => {
            let name = call
                .name_ref()
                .map(|n| n.syntax().text().to_string())
                .unwrap_or_default();
            emit_unresolved_call(ctx, source, &name, "unknown", unresolved, w)
        }
    }
}

fn handle_macro(
    ctx: &Ctx<'_>,
    mac: &ast::MacroCall,
    source: &str,
    unresolved: &mut HashSet<String>,
    w: &mut impl Write,
) {
    let name = mac
        .path()
        .map(|p| p.syntax().text().to_string())
        .unwrap_or_default();
    if name.is_empty() {
        return;
    }
    let category = match ctx.sema.resolve_macro_call(mac) {
        Some(m) => {
            let krate = m.module(ctx.db).krate(ctx.db);
            crate_category(ctx, krate)
        }
        None => path_category(&name),
    };
    emit_unresolved_call(ctx, source, &name, category, unresolved, w)
}

fn handle_type(
    ctx: &Ctx<'_>,
    ty: &ast::Type,
    source: &str,
    eps: &Endpoints<'_>,
    unresolved: &mut HashSet<String>,
    w: &mut impl Write,
) {
    let Some(resolved) = ctx.sema.resolve_type(ty) else {
        return;
    };
    // Only named ADTs are interesting; primitives/refs/params fall through.
    let Some(adt) = resolved.autoderef(ctx.db).find_map(|t| t.as_adt()) else {
        return;
    };
    emit_type_use(ctx, adt, source, eps, unresolved, w);
}

fn handle_record(
    ctx: &Ctx<'_>,
    rec_expr: &ast::RecordExpr,
    source: &str,
    eps: &Endpoints<'_>,
    unresolved: &mut HashSet<String>,
    w: &mut impl Write,
) {
    if let Some(path) = rec_expr.path() {
        if let Some(PathResolution::Def(ModuleDef::Adt(adt))) = ctx.sema.resolve_path(&path) {
            emit_type_use(ctx, adt, source, eps, unresolved, w);
            return;
        }
    }
    if let Some(variant) = ctx.sema.resolve_variant(rec_expr.clone()) {
        match variant {
            hir::Variant::Struct(s) => {
                if let Some(key) = s
                    .source(ctx.db)
                    .and_then(|src| source_key(ctx, src))
                    .and_then(|k| eps.struct_id(&k))
                {
                    edge(
                        w,
                        Rec::Uses {
                            from: source.to_string(),
                            to: key,
                        },
                    );
                }
            }
            hir::Variant::EnumVariant(v) => {
                if let Some(key) = v
                    .source(ctx.db)
                    .and_then(|src| source_key(ctx, src))
                    .and_then(|k| eps.struct_id(&k))
                {
                    edge(
                        w,
                        Rec::Uses {
                            from: source.to_string(),
                            to: key,
                        },
                    );
                }
            }
            hir::Variant::Union(_) => {}
        }
    }
}

/// calls edge to a project function by (path, start); unresolved otherwise.
fn emit_call(
    ctx: &Ctx<'_>,
    f: Function,
    source: &str,
    eps: &Endpoints<'_>,
    unresolved: &mut HashSet<String>,
    w: &mut impl Write,
) {
    let key = f.source(ctx.db).and_then(|src| source_key(ctx, src));
    if let Some(key) = key {
        if let Some(tgt) = eps.fn_id(&key) {
            edge(
                w,
                Rec::Calls {
                    from: source.to_string(),
                    to: tgt,
                },
            );
            return;
        }
    }
    // Foreign (or macro-generated): record by the module-qualified name.
    let module = f.module(ctx.db);
    let fqn = format!(
        "{}.{}",
        module_fqn_full(ctx, module),
        f.name(ctx.db).as_str()
    );
    let cat = crate_category(ctx, module.krate(ctx.db));
    emit_unresolved_call(ctx, source, &fqn, cat, unresolved, w);
}

/// uses edge to a project ADT; unresolved_use for a foreign one.
fn emit_type_use(
    ctx: &Ctx<'_>,
    adt: Adt,
    source: &str,
    eps: &Endpoints<'_>,
    unresolved: &mut HashSet<String>,
    w: &mut impl Write,
) {
    let key = adt.source(ctx.db).and_then(|src| source_key(ctx, src));
    if let Some(key) = key {
        if let Some(tgt) = eps.struct_id(&key) {
            edge(
                w,
                Rec::Uses {
                    from: source.to_string(),
                    to: tgt,
                },
            );
            return;
        }
    }
    let fqn = adt_self_fqn(ctx, adt).unwrap_or_default();
    if fqn.is_empty() {
        return;
    }
    let cat = crate_category(ctx, adt.module(ctx.db).krate(ctx.db));
    emit_u_node(ctx, w, unresolved, &fqn, cat);
    edge(
        w,
        Rec::UnresolvedUse {
            from: source.to_string(),
            to: fqn,
        },
    );
}

fn emit_unresolved_call(
    ctx: &Ctx<'_>,
    source: &str,
    target: &str,
    category: &'static str,
    unresolved: &mut HashSet<String>,
    w: &mut impl Write,
) {
    if source.is_empty() || target.is_empty() {
        return;
    }
    emit_u_node(ctx, w, unresolved, target, category);
    edge(
        w,
        Rec::UnresolvedCall {
            from: source.to_string(),
            to: target.to_string(),
            target_type: String::new(),
        },
    );
}

fn emit_u_node(
    _ctx: &Ctx<'_>,
    w: &mut impl Write,
    unresolved: &mut HashSet<String>,
    fqn: &str,
    category: &str,
) {
    if fqn.is_empty() || !unresolved.insert(fqn.to_string()) {
        return;
    }
    let _ = writeln!(
        w,
        "{}",
        serde_json::to_string(&Rec::Unresolved {
            fqn: fqn.to_string(),
            category: Some(category.to_string()),
        })
        .unwrap()
    );
}

fn edge<W: Write>(w: &mut W, r: Rec) {
    let _ = writeln!(w, "{}", serde_json::to_string(&r).unwrap());
}
