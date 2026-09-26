//! JSONL record and edge emission helpers.

use std::collections::HashSet;
use std::io::Write;

use crate::load::Ctx;
use crate::record::Rec;
use crate::state::State;

pub(crate) fn rec<W: Write>(w: &mut W, r: Rec) {
    let _ = writeln!(w, "{}", serde_json::to_string(&r).unwrap());
}

pub(crate) fn emit_unresolved<W: Write>(state: &mut State, w: &mut W, fqn: &str, category: &str) {
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

pub(crate) fn emit_unresolved_call(
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

pub(crate) fn emit_u_node(
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

pub(crate) fn edge<W: Write>(w: &mut W, r: Rec) {
    let _ = writeln!(w, "{}", serde_json::to_string(&r).unwrap());
}
