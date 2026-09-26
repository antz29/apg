//! Declaration extraction: functions, ADTs, variants, traits, callable params,
//! self-type FQNs and impl processing.

use hir::{Adt, AssocItem, Function, HasSource, Impl, Trait};
use syntax::ast::{self, AstNode};

use crate::identity::module_fqn_full;
use crate::load::Ctx;
use crate::record::{Decl, ImplEdge};
use crate::source::item_loc;
use crate::state::{push_decl, State};

pub(crate) fn fn_decl(ctx: &Ctx<'_>, f: Function, parent: &str) -> Option<Decl> {
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

pub(crate) fn adt_decl(ctx: &Ctx<'_>, adt: Adt, parent: &str) -> Option<Decl> {
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

pub(crate) fn variant_decl(ctx: &Ctx<'_>, v: hir::EnumVariant, parent: &str) -> Option<Decl> {
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

pub(crate) fn trait_decl(ctx: &Ctx<'_>, t: Trait, parent: &str) -> Option<Decl> {
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

pub(crate) fn fn_params(f: &ast::Fn) -> Vec<String> {
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

pub(crate) fn self_type_fqn(ctx: &Ctx<'_>, imp: Impl) -> Option<String> {
    let ty = imp.self_ty(ctx.db);
    let adt = ty.autoderef(ctx.db).find_map(|t| t.as_adt())?;
    adt_self_fqn(ctx, adt)
}

/// `moduleFQN.name` for an ADT, project or foreign.
pub(crate) fn adt_self_fqn(ctx: &Ctx<'_>, adt: Adt) -> Option<String> {
    Some(format!(
        "{}.{}",
        module_fqn_full(ctx, adt.module(ctx.db)),
        adt.name(ctx.db).as_str()
    ))
}

pub(crate) fn process_impl(
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
