use super::*;
use crate::graph::{Location, Node};
use std::path::PathBuf;

fn node(kind: NodeKind, path: &str) -> Node {
    Node {
        kind,
        location: Some(Location {
            path: PathBuf::from(path),
            start: 0,
            end: 1,
            start_line: 1,
            end_line: 1,
        }),
        ..Node::default()
    }
}

fn put(g: &mut Graph, fqn: &str, kind: NodeKind, path: &str) {
    g.nodes.insert(fqn.to_string(), node(kind, path));
}

/// unit tier -- pure in-memory: no filesystem, database, git or process.
/// (This module has no e2e test: every test operates on in-memory graphs, so
/// no `e2e` submodule exists.)
mod unit {
    use super::*;

    #[test]
    fn granularity_is_decided_per_language() {
        assert_eq!(granularity("java"), Granularity::Package);
        assert_eq!(granularity("go"), Granularity::Package);
        assert_eq!(granularity("rust"), Granularity::Crate);
        assert_eq!(granularity("ts"), Granularity::File);
        assert_eq!(granularity("csharp"), Granularity::Project);
        assert_eq!(granularity("python"), Granularity::PackageOrModule);
        assert_eq!(granularity("cpp"), Granularity::File);
        assert_eq!(granularity("markdown"), Granularity::File);
        assert_eq!(granularity("unknown-lang"), Granularity::File);
    }

    #[test]
    fn reverse_dep_closure_pulls_dependents() {
        // a.go declares A; b.go depends on A; c.go depends on b.
        let mut g = Graph::default();
        put(&mut g, "m.A", NodeKind::Struct, "/r/a.go");
        put(&mut g, "m.B", NodeKind::Struct, "/r/b.go");
        put(&mut g, "m.C", NodeKind::Struct, "/r/c.go");
        put(&mut g, "/r/a.go", NodeKind::File, "/r/a.go");
        put(&mut g, "/r/b.go", NodeKind::File, "/r/b.go");
        put(&mut g, "/r/c.go", NodeKind::File, "/r/c.go");
        // b uses A, c uses B (file-level dependency chain).
        g.uses.insert(("m.B".into(), "m.A".into()));
        g.uses.insert(("m.C".into(), "m.B".into()));
        let idx = DepIndex::from_graph(&g);
        assert!(idx.forward["/r/b.go"].contains("/r/a.go"));
        let closure = idx.reverse_closure(&BTreeSet::from(["/r/a.go".to_string()]));
        assert!(closure.contains("/r/a.go"));
        assert!(closure.contains("/r/b.go"));
        assert!(closure.contains("/r/c.go"));

        // A leaf change with a signature change emits the leaf + dependents.
        let changed = BTreeSet::from(["/r/a.go".to_string()]);
        let ts = target_set(&changed, &changed, &idx, &g);
        assert!(ts.contains("/r/a.go"));
        assert!(ts.contains("/r/b.go"));
        assert!(ts.contains("/r/c.go"));
        assert!(ts.dependents.contains("/r/b.go"));

        // A body-only change is cut off: only the leaf is emitted.
        let body_only = BTreeSet::new();
        let ts = target_set(&changed, &body_only, &idx, &g);
        assert_eq!(ts.files, changed);
        assert!(ts.dependents.is_empty());
    }

    #[test]
    fn overload_group_peers_are_emitted() {
        // Two overloads of m.C.foo live in a.go and b.go; changing one must emit
        // the whole scope's files.
        let mut g = Graph::default();
        put(&mut g, "m.C.foo(int)", NodeKind::Function, "/r/a.go");
        put(&mut g, "m.C.foo(string)", NodeKind::Function, "/r/b.go");
        put(&mut g, "/r/a.go", NodeKind::File, "/r/a.go");
        put(&mut g, "/r/b.go", NodeKind::File, "/r/b.go");
        g.calls
            .insert(("m.C.foo(int)".into(), "m.C.foo(string)".into()));
        let scopes = overload_groups_of_file(&g, "/r/a.go");
        assert!(scopes.contains("m.C"));
        let peers = overload_peer_files(&g, &scopes);
        assert!(peers.contains("/r/a.go"));
        assert!(peers.contains("/r/b.go"));

        let idx = DepIndex::from_graph(&g);
        let changed = BTreeSet::from(["/r/a.go".to_string()]);
        let ts = target_set(&changed, &changed, &idx, &g);
        assert!(ts.overload_peers.contains("/r/b.go"));
        assert!(ts.contains("/r/b.go"));

        // A newly-added overload: the changed file declares only the new
        // signature, but its scope already holds the existing sibling in b.go —
        // the existing peer is still emitted (scope-based, not group-based).
        let mut new_graph = g.clone();
        put(
            &mut new_graph,
            "m.C.foo(bool)",
            NodeKind::Function,
            "/r/a.go",
        );
        let scopes = overload_groups_of_file(&new_graph, "/r/a.go");
        assert!(scopes.contains("m.C"));
        let peers = overload_peer_files(&new_graph, &scopes);
        assert!(
            peers.contains("/r/b.go"),
            "the existing sibling must re-emit"
        );
    }

    #[test]
    fn signature_cutoff_distinguishes_body_and_signature_changes() {
        let mut old = Graph::default();
        put(&mut old, "m.C.foo", NodeKind::Function, "/r/a.go");
        put(&mut old, "m.C.bar", NodeKind::Function, "/r/a.go");
        let mut newb = old.clone();
        // Body-only: no declaration surface change.
        assert!(signature_unchanged(&old, &newb, "/r/a.go"));

        // Signature change: an added declaration / changed params.
        put(&mut newb, "m.C.baz", NodeKind::Function, "/r/a.go");
        assert!(!signature_unchanged(&old, &newb, "/r/a.go"));

        // A changed arity: foo() -> foo(int) re-suffixes the overload.
        let mut old2 = Graph::default();
        put(&mut old2, "m.C.foo", NodeKind::Function, "/r/a.go");
        let mut new2 = Graph::default();
        put(&mut new2, "m.C.foo(int)", NodeKind::Function, "/r/a.go");
        assert!(!signature_unchanged(&old2, &new2, "/r/a.go"));
    }
}
