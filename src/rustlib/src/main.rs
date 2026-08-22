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
use project_model::{CargoConfig, CargoFeatures, ProjectManifest, ProjectWorkspace, RustLibSource};
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
            unresolved_seen: HashSet::new(),
            impl_edges: Vec::new(),
        }
    }
}

struct Ctx<'db> {
    db: &'db RootDatabase,
    sema: Semantics<'db, RootDatabase>,
    vfs: &'db Vfs,
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
            "Usage: rustfrontend <dir> [--module <dir>]... [--no-build-scripts] [exclude...]"
        );
        std::process::exit(1);
    }
    let root = args[1].clone();
    let mut module_dirs: Vec<String> = Vec::new();
    let mut excludes: Vec<String> = Vec::new();
    let mut no_build_scripts = false;
    {
        let mut i = 2;
        while i < args.len() {
            let a = args[i].as_str();
            if a == "--module" && i + 1 < args.len() {
                module_dirs.push(args[i + 1].clone());
                i += 1;
            } else if a == "--no-build-scripts" {
                no_build_scripts = true;
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

    let (db, vfs) = load_workspace_at(&root_abs, no_build_scripts)?;
    let sema = Semantics::new(&db);
    let ctx = Ctx {
        db: &db,
        sema,
        vfs: &vfs,
    };

    // Type inference (method resolution, type resolution) interns through a
    // thread-local db; run the whole scan inside it.
    hir::attach_db(ctx.db, || scan(ctx, &root_abs, module_dirs, excludes))
}

fn scan(
    ctx: Ctx<'_>,
    root_abs: &std::path::Path,
    module_dirs: Vec<String>,
    excludes: Vec<String>,
) -> Result<()> {
    let mut state = State::new();
    let out = std::io::stdout();
    let mut w = std::io::BufWriter::new(out.lock());

    // ── Pass 1a: module records + Module→Module containment, per crate. ──
    let mut crates: Vec<Crate> = Crate::all(ctx.db)
        .into_iter()
        .filter(|k| k.origin(ctx.db).is_local())
        .filter(|k| within_module_limit(&ctx, *k, &module_dirs))
        .collect();
    crates.sort_by_key(|k| crate_prefix(&ctx, *k));

    if crates.is_empty() {
        eprintln!(
            "Error: no Rust workspace found under {}",
            root_abs.display()
        );
        std::process::exit(1);
    }

    let mut module_fqn: HashMap<Module, String> = HashMap::new();
    let mut module_nodes: Vec<String> = Vec::new();
    let mut module_edges: Vec<(String, String)> = Vec::new();
    for krate in &crates {
        let prefix = crate_prefix(&ctx, *krate);
        let root_mod = krate.root_module(ctx.db);
        let mut seen: HashSet<Module> = HashSet::new();
        let mut stack: Vec<Module> = vec![root_mod];
        while let Some(m) = stack.pop() {
            if !seen.insert(m) {
                continue;
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
    }
    module_nodes.sort();
    module_edges.sort();
    for fqn in &module_nodes {
        rec(&mut w, Rec::Module { fqn: fqn.clone() });
    }
    for (from, to) in &module_edges {
        rec(
            &mut w,
            Rec::Contains {
                from: from.clone(),
                to: to.clone(),
            },
        );
    }

    // ── Pass 1b: collect declarations (ids assigned as they are collected). ──
    let mut decls: Vec<Decl> = Vec::new();
    let mut file_module: HashMap<FileId, String> = HashMap::new();
    for krate in &crates {
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
                file_module
                    .entry(ed.file_id(ctx.db))
                    .or_insert(mod_fqn.clone());
            }

            for def in m.declarations(ctx.db) {
                match def {
                    ModuleDef::Function(f) => {
                        if let Some(d) = fn_decl(&ctx, f, &mod_fqn) {
                            push_decl(&mut state, &mut decls, d);
                        }
                    }
                    ModuleDef::Adt(adt) => {
                        if let Some(d) = adt_decl(&ctx, adt, &mod_fqn) {
                            push_decl(&mut state, &mut decls, d);
                        }
                        if let Adt::Enum(e) = adt {
                            // Enum variants hang under the enum.
                            let enum_fqn = format!("{}.{}", mod_fqn, e.name(ctx.db).as_str());
                            for v in e.variants(ctx.db) {
                                if let Some(d) = variant_decl(&ctx, v, &enum_fqn) {
                                    push_decl(&mut state, &mut decls, d);
                                }
                            }
                        }
                    }
                    ModuleDef::Trait(t) => {
                        if let Some(d) = trait_decl(&ctx, t, &mod_fqn) {
                            push_decl(&mut state, &mut decls, d);
                        }
                        // Trait methods (declarations and defaults) hang under
                        // the trait, like Go interface methods under a type.
                        let trait_fqn = format!("{}.{}", mod_fqn, t.name(ctx.db).as_str());
                        for item in t.items(ctx.db) {
                            if let AssocItem::Function(f) = item {
                                if let Some(d) = fn_decl(&ctx, f, &trait_fqn) {
                                    push_decl(&mut state, &mut decls, d);
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            for imp in m.impl_defs(ctx.db) {
                process_impl(&ctx, imp, &mod_fqn, &mut state, &mut decls);
            }
            stack.extend(m.children(ctx.db));
        }
    }
    drop(crates);

    decls.sort_by_key(|d| (d.path.clone(), d.start));

    // Assign opaque ids in sorted emission order (deterministic across runs),
    // and register the id maps used by pass 2 and structural edges.
    for d in &mut decls {
        state.next_id += 1;
        d.id = format!("n{}", state.next_id);
        let fqn = format!("{}.{}", d.parent, d.name);
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
    let total_files = files
        .iter()
        .filter(|f| !path_excluded(&path_of(&ctx, **f), &excludes))
        .count();
    let mut scanned = 0usize;
    for fid in &files {
        let path = path_of(&ctx, *fid);
        if path_excluded(&path, &excludes) {
            continue;
        }
        let text = ctx.db.file_text(*fid).text(ctx.db).to_string();
        let parent = file_module.get(fid).cloned().unwrap_or_default();
        rec(
            &mut w,
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
        let _ = writeln!(w, "{}", node_record(d));
    }

    // Structural containment: a unit whose parent is a struct-like node hangs
    // under it (methods under self type, trait methods under trait, enum
    // variants under enum).
    for d in &decls {
        if let Some(pid) = state.struct_id.get(&d.parent) {
            rec(
                &mut w,
                Rec::Contains {
                    from: pid.clone(),
                    to: d.id.clone(),
                },
            );
        }
    }

    // `impl Trait for Self`: Uses to a project trait, UnresolvedUse to a
    // foreign one. Only when the self type is a project struct.
    let impl_edges = std::mem::take(&mut state.impl_edges);
    for ie in &impl_edges {
        let Some(from) = state.struct_id.get(&ie.self_fqn).cloned() else {
            continue;
        };
        if let Some(to) = state.struct_id.get(&ie.trait_fqn) {
            rec(
                &mut w,
                Rec::Uses {
                    from: from.clone(),
                    to: to.clone(),
                },
            );
        } else {
            emit_unresolved(&mut state, &mut w, &ie.trait_fqn, "external");
            rec(
                &mut w,
                Rec::UnresolvedUse {
                    from,
                    to: ie.trait_fqn.clone(),
                },
            );
        }
    }

    // ── Pass 2: edge records from a syntax walk of every project file. ──
    for fid in &files {
        let path = path_of(&ctx, *fid);
        if path_excluded(&path, &excludes) {
            continue;
        }
        let _ = ctx.db.file_text(*fid).text(ctx.db);
        walk_file(&ctx, *fid, &mut state, &mut w);
    }
    let _ = w.flush();
    Ok(())
}

fn push_decl(state: &mut State, decls: &mut Vec<Decl>, d: Decl) {
    // Ids are assigned later, in sorted (path, start) order, for deterministic
    // output. Until then the decl carries an empty id.
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

// ── workspace load ────────────────────────────────────────────────────

fn load_workspace_at(
    root: &std::path::Path,
    no_build_scripts: bool,
) -> Result<(RootDatabase, Vfs)> {
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
            let ws = ProjectWorkspace::load(manifest, &cargo_config, &progress)?;
            ws
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

    let extra_env: FxHashMap<String, Option<String>> = FxHashMap::default();
    let (db, vfs, _) = load_workspace(ws, &extra_env, &load_config)?;
    Ok((db, vfs))
}

// ── FQN / path helpers ────────────────────────────────────────────────

fn crate_prefix(ctx: &Ctx<'_>, krate: Crate) -> String {
    if let Some(display) = krate.display_name(ctx.db) {
        return display.to_string();
    }
    if let Some(name) = krate.root_module(ctx.db).name(ctx.db) {
        return name.as_str().to_string();
    }
    "crate".to_string()
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
    ctx.vfs
        .file_path(file_id)
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

/// (path, start, end, start_line, end_line, src_key) from a real-file item's
/// span. Macro-generated items return `None` and are skipped.
fn item_loc<A: AstNode>(
    ctx: &Ctx<'_>,
    f: InFile<A>,
) -> Option<(String, u32, u32, u32, u32, (String, u32))> {
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

fn process_impl(ctx: &Ctx<'_>, imp: Impl, mod_fqn: &str, state: &mut State, decls: &mut Vec<Decl>) {
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
                push_decl(state, decls, d);
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

fn walk_file(ctx: &Ctx<'_>, file_id: FileId, state: &mut State, w: &mut impl Write) {
    let sf = ctx.sema.parse_guess_edition(file_id);
    let node = sf.syntax().clone();
    walk_node(
        ctx,
        &node,
        None,
        &state.id_by_source,
        &state.struct_sources,
        &mut state.unresolved_seen,
        w,
    );
}

fn walk_node(
    ctx: &Ctx<'_>,
    node: &syntax::SyntaxNode,
    cur: Option<String>,
    id_by_source: &HashMap<(String, u32), String>,
    struct_sources: &HashMap<(String, u32), String>,
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
            .and_then(|k| id_by_source.get(&k).cloned())
            .or(cur);
        for child in node.children() {
            walk_node(
                ctx,
                &child,
                new_cur.clone(),
                id_by_source,
                struct_sources,
                unresolved,
                w,
            );
        }
        return;
    }
    if let Some(c) = current_item_id_for(ctx, struct_sources, node.clone()) {
        for child in node.children() {
            walk_node(
                ctx,
                &child,
                Some(c.clone()),
                id_by_source,
                struct_sources,
                unresolved,
                w,
            );
        }
        return;
    }

    // Edge extraction for nodes directly in the current context.
    if let Some(c) = cur.as_ref() {
        if let Some(call) = ast::CallExpr::cast(node.clone()) {
            handle_call(ctx, &call, c, id_by_source, struct_sources, unresolved, w);
        } else if let Some(mc) = ast::MethodCallExpr::cast(node.clone()) {
            handle_method_call(ctx, &mc, c, id_by_source, unresolved, w);
        } else if let Some(mac) = ast::MacroCall::cast(node.clone()) {
            handle_macro(ctx, &mac, c, unresolved, w);
        } else if let Some(ty) = ast::Type::cast(node.clone()) {
            handle_type(ctx, &ty, c, struct_sources, unresolved, w);
        } else if let Some(re) = ast::RecordExpr::cast(node.clone()) {
            handle_record(ctx, &re, c, id_by_source, struct_sources, unresolved, w);
        }
    }
    for child in node.children() {
        walk_node(
            ctx,
            &child,
            cur.clone(),
            id_by_source,
            struct_sources,
            unresolved,
            w,
        );
    }
}

/// If `node` is a struct/enum/union/trait whose id is a project struct node,
/// returns that id (so field types and trait bounds attribute their edges to
/// the struct itself).
fn current_item_id_for(
    ctx: &Ctx<'_>,
    struct_sources: &HashMap<(String, u32), String>,
    node: syntax::SyntaxNode,
) -> Option<String> {
    let adt = if let Some(s) = ast::Struct::cast(node.clone()) {
        ctx.sema.to_struct_def(&s).map(Adt::Struct)
    } else if let Some(s) = ast::Enum::cast(node.clone()) {
        ctx.sema.to_enum_def(&s).map(Adt::Enum)
    } else if let Some(s) = ast::Union::cast(node.clone()) {
        ctx.sema.to_union_def(&s).map(Adt::Union)
    } else if let Some(t) = ast::Trait::cast(node.clone()) {
        let d = ctx.sema.to_trait_def(&t)?;
        return source_key(ctx, d.source(ctx.db)?).and_then(|k| struct_sources.get(&k).cloned());
    } else {
        return None;
    };
    let d = adt?;
    let src = d.source(ctx.db)?;
    source_key(ctx, src).and_then(|k| struct_sources.get(&k).cloned())
}

// ── edge handlers ─────────────────────────────────────────────────────

fn handle_call(
    ctx: &Ctx<'_>,
    call: &ast::CallExpr,
    source: &str,
    id_by_source: &HashMap<(String, u32), String>,
    struct_sources: &HashMap<(String, u32), String>,
    unresolved: &mut HashSet<String>,
    w: &mut impl Write,
) {
    let Some(callee) = call.expr() else { return };
    match callee {
        ast::Expr::PathExpr(pe) => {
            let Some(path) = pe.path() else { return };
            match ctx.sema.resolve_path(&path) {
                Some(PathResolution::Def(ModuleDef::Function(f))) => {
                    emit_call(ctx, f, source, id_by_source, unresolved, w)
                }
                Some(PathResolution::Def(ModuleDef::Adt(adt))) => {
                    emit_type_use(ctx, adt, source, struct_sources, unresolved, w)
                }
                Some(PathResolution::Def(ModuleDef::EnumVariant(v))) => {
                    if let Some(key) = v
                        .source(ctx.db)
                        .and_then(|src| source_key(ctx, src))
                        .and_then(|k| id_by_source.get(&k).cloned())
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
    id_by_source: &HashMap<(String, u32), String>,
    unresolved: &mut HashSet<String>,
    w: &mut impl Write,
) {
    match ctx.sema.resolve_method_call(call) {
        Some(f) => emit_call(ctx, f, source, id_by_source, unresolved, w),
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
        .unwrap_or_else(|| String::new());
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
    struct_sources: &HashMap<(String, u32), String>,
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
    emit_type_use(ctx, adt, source, struct_sources, unresolved, w);
}

fn handle_record(
    ctx: &Ctx<'_>,
    rec_expr: &ast::RecordExpr,
    source: &str,
    id_by_source: &HashMap<(String, u32), String>,
    struct_sources: &HashMap<(String, u32), String>,
    unresolved: &mut HashSet<String>,
    w: &mut impl Write,
) {
    if let Some(path) = rec_expr.path() {
        if let Some(PathResolution::Def(ModuleDef::Adt(adt))) = ctx.sema.resolve_path(&path) {
            emit_type_use(ctx, adt, source, struct_sources, unresolved, w);
            return;
        }
    }
    if let Some(variant) = ctx.sema.resolve_variant(rec_expr.clone()) {
        match variant {
            hir::Variant::Struct(s) => {
                if let Some(key) = s
                    .source(ctx.db)
                    .and_then(|src| source_key(ctx, src))
                    .and_then(|k| id_by_source.get(&k).cloned())
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
                    .and_then(|k| id_by_source.get(&k).cloned())
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
    id_by_source: &HashMap<(String, u32), String>,
    unresolved: &mut HashSet<String>,
    w: &mut impl Write,
) {
    let key = f.source(ctx.db).and_then(|src| source_key(ctx, src));
    if let Some(key) = key {
        if let Some(tgt) = id_by_source.get(&key) {
            edge(
                w,
                Rec::Calls {
                    from: source.to_string(),
                    to: tgt.clone(),
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
    struct_sources: &HashMap<(String, u32), String>,
    unresolved: &mut HashSet<String>,
    w: &mut impl Write,
) {
    let key = adt.source(ctx.db).and_then(|src| source_key(ctx, src));
    if let Some(key) = key {
        if let Some(tgt) = struct_sources.get(&key) {
            edge(
                w,
                Rec::Uses {
                    from: source.to_string(),
                    to: tgt.clone(),
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
