use crate::classify::matches_glob;
use crate::graph::{Graph, NodeKind};

pub struct CleanupOptions {
    pub user_excludes: Vec<String>,
    /// Language of the scan. Span validation is Java-only: Go methods are
    /// declared outside the struct body (receiver-based), so their spans
    /// legitimately fall outside the struct's span.
    pub language: String,
}

pub struct CleanupReport {
    pub nodes_removed: usize,
    pub contains_removed: usize,
    pub calls_removed: usize,
    pub uses_removed: usize,
    pub unresolved_calls_removed: usize,
    pub unresolved_uses_removed: usize,
    pub span_violations_removed: usize,
}

fn path_excluded(path: &str, opts: &CleanupOptions) -> bool {
    opts.user_excludes
        .iter()
        .any(|pat| matches_glob(pat, path))
}

pub fn cleanup(graph: &mut Graph, opts: &CleanupOptions) -> CleanupReport {
    let mut nodes_removed = 0;
    let before_contains = graph.contains.len();
    let before_calls = graph.calls.len();
    let before_uses = graph.uses.len();
    let before_u_calls = graph.unresolved_calls.len();
    let before_u_uses = graph.unresolved_uses.len();

    let removed: Vec<String> = graph
        .nodes
        .iter()
        .filter(|(_, n)| {
            matches!(n.kind, NodeKind::Struct | NodeKind::Function)
                && n.location
                    .as_ref()
                    .map(|l| path_excluded(&l.path.to_string_lossy(), opts))
                    .unwrap_or(false)
        })
        .map(|(fqn, _)| fqn.clone())
        .collect();
    for fqn in &removed {
        graph.nodes.remove(fqn);
        nodes_removed += 1;
    }
    let is_removed = |fqn: &String| graph.nodes.contains_key(fqn);

    graph.contains.retain(|(a, b)| is_removed(a) && is_removed(b));
    graph.calls.retain(|(a, b)| is_removed(a) && is_removed(b));
    graph.uses.retain(|(a, b)| is_removed(a) && is_removed(b));
    graph
        .unresolved_calls
        .retain(|(a, b, _)| is_removed(a) && is_removed(b));
    graph
        .unresolved_uses
        .retain(|(a, b)| is_removed(a) && is_removed(b));

    // Containment span validation: a Struct may only contain Functions whose
    // start offset falls inside the struct's span. Java-only — Go methods are
    // declared outside the struct body, so the check would wrongly drop them.
    let mut span_violations = 0usize;
    if opts.language == "java" {
        graph.contains.retain(|(a, b)| {
            let Some(na) = graph.nodes.get(a) else { return false };
            let Some(nb) = graph.nodes.get(b) else { return false };
            if na.kind != NodeKind::Struct || nb.kind != NodeKind::Function {
                return true;
            }
            let (Some(sa), Some(sb)) = (na.location.as_ref(), nb.location.as_ref()) else {
                return true;
            };
            if sb.start < sa.start || sb.start > sa.end {
                span_violations += 1;
                return false;
            }
            true
        });
    }

    CleanupReport {
        nodes_removed,
        contains_removed: before_contains - graph.contains.len(),
        calls_removed: before_calls - graph.calls.len(),
        uses_removed: before_uses - graph.uses.len(),
        unresolved_calls_removed: before_u_calls - graph.unresolved_calls.len(),
        unresolved_uses_removed: before_u_uses - graph.unresolved_uses.len(),
        span_violations_removed: span_violations,
    }
}
