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

/// The mixed rust/py/ps overload fixture; `ps_f` is the unique-form `//`
/// function in `/r/a/b.ps1` (renamed between graphs by the rename test).
fn mixed_overload_graph(ps_f: &str) -> Graph {
    let mut g = Graph::default();
    for (fqn, path) in [
        ("rust.apg.x.f", "/r/src/x.rs"),
        ("rust.apg.T.m(i32)", "/r/src/x.rs"),
        ("py.pkg.mod.g", "/r/pkg/mod.py"),
        ("ps.m.s.ps1.C.M(int)//d.(x)/c.ps1:3", "/r/d.(x)/c.ps1"),
        ("ps..s.ps1.F//a.b/c.ps1:3", "/r/a.b/c.ps1"),
        ("ps.m.s.ps1.C.M", "/r/m/s.ps1"),
        (ps_f, "/r/a/b.ps1"),
    ] {
        put(&mut g, fqn, NodeKind::Function, path);
    }
    g
}

fn is_ps_root(s: &str) -> bool {
    s == "ps" || s.starts_with("ps.")
}

/// Asserts the ps/non-ps overload scopes and peer files never cross languages.
fn assert_ps_peers_sound(idx: &crate::impact::OverloadIndex, ps_files: &[&str]) {
    let (ps, other): (Vec<_>, Vec<_>) = idx
        .scopes
        .iter()
        .partition(|(_, fqns)| fqns.iter().any(|f| f.starts_with("ps.")));
    assert!(!ps.is_empty() && !other.is_empty());
    for (scope, _) in &ps {
        assert!(is_ps_root(scope), "ps scope {scope} is not ps-rooted");
        assert!(
            other.iter().all(|(o, _)| o != scope),
            "ps scope {scope} collides with a non-ps scope"
        );
    }
    for f in ps_files {
        let peers = idx.peer_files(f);
        assert!(peers.contains(*f), "{f} must be its own peer");
        for p in &peers {
            assert_eq!(crate::incremental::language_of(p), "ps", "{f} peer {p}");
        }
    }
    for f in ["src/x.rs", "pkg/mod.py"] {
        let peers = idx.peer_files(f);
        assert!(peers.contains(f));
        for p in &peers {
            assert_ne!(crate::incremental::language_of(p), "ps", "{f} peer {p}");
        }
    }
}

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
        assert_eq!(granularity("cobol"), Granularity::File);
        assert_eq!(granularity("sql"), Granularity::Language);
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

    #[test]
    fn ps_overload_scopes_are_ps_rooted_and_disjoint() {
        // overload_key's plain string split may mis-parent a ps FQN (a `(`
        // or `.` inside the `//` suffix); that is sound while every derived
        // scope stays ps-rooted and so never meets a rust/py scope.
        let g = mixed_overload_graph("ps.a/b.ps1//a/b.ps1.F");
        let idx = OverloadIndex::from_graph(&g, std::path::Path::new("/r"));
        assert_ps_peers_sound(&idx, &["d.(x)/c.ps1", "a.b/c.ps1", "m/s.ps1", "a/b.ps1"]);
    }

    #[test]
    fn ps_renamed_function_scopes_stay_ps_rooted_across_graphs() {
        for f in ["ps.a/b.ps1//a/b.ps1.F", "ps.a/b.ps1//a/b.ps1.G"] {
            let g = mixed_overload_graph(f);
            let idx = OverloadIndex::from_graph(&g, std::path::Path::new("/r"));
            let (scope, _) = idx
                .scopes
                .iter()
                .find(|(_, fqns)| fqns.iter().any(|x| x == f))
                .expect("renamed function has a scope");
            assert!(is_ps_root(scope), "{f} scope {scope}");
            assert_ps_peers_sound(&idx, &["a/b.ps1"]);
        }
    }
}
