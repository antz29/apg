//! Scan orchestration: the project discovery loop (`run`) and per-project
//! emission (`scan`).

use std::collections::{HashMap, HashSet};
use std::io::Write;

use anyhow::Result;
use hir::{Adt, AssocItem, Crate, Module, ModuleDef, Semantics};
use ide_db::base_db::SourceDatabase;
use vfs::FileId;

use crate::decl::{adt_decl, fn_decl, process_impl, trait_decl, variant_decl};
use crate::discovery::{
    discover_manifest_dirs, ensure_artifact_dir, local_crate_root_files, read_target_set,
    under_excluded_tree,
};
use crate::emit::{emit_unresolved, rec};
use crate::identity::{
    crate_prefix, module_fqn_with_prefix, package_prefix_map, within_module_limit,
};
use crate::load::{load_project, Ctx};
use crate::record::{node_record, Decl, Rec};
use crate::source::{line_count, path_excluded, path_of};
use crate::state::{push_decl, Endpoints, State};
use crate::walk::walk_file;

pub(crate) fn run(args: Vec<String>) -> Result<()> {
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
pub(crate) fn scan(
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
