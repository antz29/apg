//! The pass-2 syntax walk and call/type-edge emission.

use std::collections::HashSet;
use std::io::Write;
use std::path::Path;

use hir::{Adt, Crate, Function, HasSource, ModuleDef, PathResolution};
use ide_db::base_db::CrateOrigin;
use syntax::ast::{self, AstNode};
use vfs::FileId;

use crate::decl::adt_self_fqn;
use crate::emit::{edge, emit_u_node, emit_unresolved_call};
use crate::identity::module_fqn_full;
use crate::load::Ctx;
use crate::record::Rec;
use crate::source::{path_of, source_key};
use crate::state::Endpoints;

pub(crate) fn walk_file(
    ctx: &Ctx<'_>,
    file_id: FileId,
    root: &Path,
    eps: &Endpoints<'_>,
    unresolved: &mut HashSet<String>,
    w: &mut impl Write,
) {
    let sf = ctx.sema.parse_guess_edition(file_id);
    let node = sf.syntax().clone();
    let abs = path_of(ctx, file_id);
    let p = Path::new(&abs);
    let rel = p
        .strip_prefix(root)
        .unwrap_or_else(|_| Path::new(p.file_name().unwrap_or_default()));
    let rel_path = rel.to_string_lossy().replace('\\', "/");
    walk_node(ctx, &node, None, &rel_path, eps, unresolved, w);
}

pub(crate) fn walk_node(
    ctx: &Ctx<'_>,
    node: &syntax::SyntaxNode,
    cur: Option<String>,
    rel_path: &str,
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
            walk_node(ctx, &child, new_cur.clone(), rel_path, eps, unresolved, w);
        }
        return;
    }
    if let Some(c) = current_item_id_for(ctx, eps, node.clone()) {
        for child in node.children() {
            walk_node(ctx, &child, Some(c.clone()), rel_path, eps, unresolved, w);
        }
        return;
    }

    // Edge extraction for nodes directly in the current context.
    if let Some(c) = cur.as_ref() {
        if let Some(call) = ast::CallExpr::cast(node.clone()) {
            handle_call(ctx, &call, c, rel_path, eps, unresolved, w);
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
        walk_node(ctx, &child, cur.clone(), rel_path, eps, unresolved, w);
    }
}

/// If `node` is a struct/enum/union/trait whose id is a project struct node,
/// returns that id (so field types and trait bounds attribute their edges to
/// the struct itself).
pub(crate) fn current_item_id_for(
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

/// The callee's identity: a name path verbatim, or a site-keyed sentinel for a
/// nameless callee expression (a place, never a span of source text).
pub(crate) fn callee_identity(callee: &ast::Expr, rel_path: &str) -> String {
    if let ast::Expr::PathExpr(pe) = callee {
        if let Some(path) = pe.path() {
            return path.syntax().text().to_string();
        }
    }
    format!(
        "<nameless@{}:{}>",
        rel_path,
        u32::from(callee.syntax().text_range().start())
    )
}

pub(crate) fn handle_call(
    ctx: &Ctx<'_>,
    call: &ast::CallExpr,
    source: &str,
    rel_path: &str,
    eps: &Endpoints<'_>,
    unresolved: &mut HashSet<String>,
    w: &mut impl Write,
) {
    let Some(callee) = call.expr() else { return };
    match &callee {
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
                    &callee_identity(&callee, rel_path),
                    "unknown",
                    unresolved,
                    w,
                ),
                Some(_) => emit_unresolved_call(
                    ctx,
                    source,
                    &callee_identity(&callee, rel_path),
                    "unknown",
                    unresolved,
                    w,
                ),
                None => {
                    let name = callee_identity(&callee, rel_path);
                    emit_unresolved_call(ctx, source, &name, "unknown", unresolved, w)
                }
            }
        }
        _ => emit_unresolved_call(
            ctx,
            source,
            &callee_identity(&callee, rel_path),
            "unknown",
            unresolved,
            w,
        ),
    }
}

pub(crate) fn handle_method_call(
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

pub(crate) fn handle_macro(
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

pub(crate) fn handle_type(
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

pub(crate) fn handle_record(
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
pub(crate) fn emit_call(
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
pub(crate) fn emit_type_use(
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

pub(crate) fn crate_category(ctx: &Ctx<'_>, krate: Crate) -> &'static str {
    match krate.origin(ctx.db) {
        CrateOrigin::Lang(_) => "stdlib",
        CrateOrigin::Library { .. } => "external",
        _ => "unknown",
    }
}

/// How an unresolvable path is classified without resolution: std-liked
/// namespace prefixes are stdlib, qualified names external, bare names unknown.
pub(crate) fn path_category(path: &str) -> &'static str {
    if path.starts_with("std::") || path.starts_with("core::") || path.starts_with("alloc::") {
        "stdlib"
    } else if path.contains("::") {
        "external"
    } else {
        "unknown"
    }
}
