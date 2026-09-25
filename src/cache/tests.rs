use super::*;
use crate::classify::{ApgConfig, CodeTypeRule, StructuralScope};
use crate::graph::Graph;

fn located_node(kind: NodeKind, path: &str) -> Node {
    Node {
        kind,
        location: Some(Location {
            path: PathBuf::from(path),
            start: 0,
            end: 1,
            start_line: 1,
            end_line: 1,
        }),
        code_type: "src".into(),
        ..Node::default()
    }
}

fn module_node() -> Node {
    Node {
        kind: NodeKind::Module,
        ..Node::default()
    }
}

/// A one-glob `types` rule for the cache-key classification test.
fn code_type_rule(name: &str, glob: &str) -> CodeTypeRule {
    CodeTypeRule {
        name: name.to_string(),
        globs: vec![glob.to_string()],
        names: vec!["Test*".to_string()],
    }
}

/// A present structural scope for the cache-key classification test.
fn structural_scope() -> StructuralScope {
    StructuralScope {
        include: vec!["**/*.yml".to_string()],
        exclude: vec!["vendor/**".to_string()],
        code_type: Some("config".to_string()),
    }
}

/// A pure in-memory fixture graph for the single-pass index property: one
/// module over three files, a module hierarchy edge (module -> module), a
/// call/use across files, an unresolved target shared by two files, two
/// edges in one file to the SAME unresolved target (within-fragment dedup),
/// and a File node with no declarations.
fn index_fixture() -> Graph {
    let mut g = Graph::default();
    g.nodes.insert("m".into(), module_node());
    g.nodes.insert("m.sub".into(), module_node());
    g.contains.insert(("m".into(), "m.sub".into()));

    // File a: a struct + two functions, one call and one use, and two
    // unresolved edges to the SAME target.
    g.nodes
        .insert("/r/a.go".into(), located_node(NodeKind::File, "/r/a.go"));
    g.nodes
        .insert("m.a.A".into(), located_node(NodeKind::Struct, "/r/a.go"));
    g.nodes
        .insert("m.a.F".into(), located_node(NodeKind::Function, "/r/a.go"));
    g.nodes
        .insert("m.a.F2".into(), located_node(NodeKind::Function, "/r/a.go"));
    g.contains.insert(("m".into(), "/r/a.go".into()));
    g.contains.insert(("/r/a.go".into(), "m.a.A".into()));
    g.contains.insert(("/r/a.go".into(), "m.a.F".into()));
    g.contains.insert(("/r/a.go".into(), "m.a.F2".into()));
    g.calls.insert(("m.a.F".into(), "m.b.G".into()));
    g.uses.insert(("m.a.F".into(), "m.b.B".into()));
    g.unresolved_calls
        .insert(("m.a.F".into(), "fmt.Println".into(), "func(...)".into()));
    g.unresolved_calls
        .insert(("m.a.F2".into(), "fmt.Println".into(), "func(...)".into()));
    g.unresolved_uses.insert(("m.a.A".into(), "os.File".into()));

    // File b: the call/use targets plus an unresolved edge to the SAME
    // target a carries (its own row, no cross-file coupling).
    g.nodes
        .insert("/r/b.go".into(), located_node(NodeKind::File, "/r/b.go"));
    g.nodes
        .insert("m.b.B".into(), located_node(NodeKind::Struct, "/r/b.go"));
    g.nodes
        .insert("m.b.G".into(), located_node(NodeKind::Function, "/r/b.go"));
    g.contains.insert(("m".into(), "/r/b.go".into()));
    g.contains.insert(("/r/b.go".into(), "m.b.B".into()));
    g.contains.insert(("/r/b.go".into(), "m.b.G".into()));
    g.calls.insert(("m.b.G".into(), "m.a.F".into()));
    g.uses.insert(("m.b.G".into(), "m.a.A".into()));
    g.unresolved_calls
        .insert(("m.b.G".into(), "fmt.Println".into(), "func(...)".into()));

    // The unresolved-target rows the fragments carry.
    g.nodes.insert(
        "fmt.Println".into(),
        Node {
            kind: NodeKind::UnresolvedTarget,
            category: Some("stdlib".into()),
            ..Node::default()
        },
    );
    g.nodes.insert(
        "os.File".into(),
        Node {
            kind: NodeKind::UnresolvedTarget,
            category: Some("external".into()),
            ..Node::default()
        },
    );

    // A File node with no declarations (only the module -> file edge).
    g.nodes.insert(
        "/r/empty.go".into(),
        located_node(NodeKind::File, "/r/empty.go"),
    );
    g.contains.insert(("m".into(), "/r/empty.go".into()));
    g
}

/// unit tier -- pure in-memory: no filesystem, database, git or process.
mod unit {
    use super::*;

    /// fix-module-identity task-13: `rebase` promotes the checkout-relative
    /// cache model to the repo-relative identity base. A stored identity
    /// (the form the cache now writes) passes through verbatim regardless of
    /// the reading worktree; a legacy ABSOLUTE path is stripped of its
    /// writer root to its `/`-separated tail — never re-joined onto the
    /// reader's root, so a unit written in one worktree composes in another.
    #[test]
    fn rebase_yields_repo_relative_identity_for_both_worktrees() {
        // A stored repo-relative identity is portable: two reader roots (two
        // worktrees at the same commit) yield the SAME identity.
        assert_eq!(rebase("/writer/wt", "/reader/wt", "src/a.rs"), "src/a.rs");
        assert_eq!(rebase("/writer/wt", "/other/wt", "src/a.rs"), "src/a.rs");
        // An identifier (a Module/Struct/Function fqn) passes through too.
        assert_eq!(rebase("/writer/wt", "/reader/wt", "go.pkg.F"), "go.pkg.F");
        // A legacy absolute path is stripped to its repo-relative tail.
        assert_eq!(
            rebase("/writer/wt", "/reader/wt", "/writer/wt/src/a.rs"),
            "src/a.rs"
        );
        // An absolute spelling of the READER's own tree normalizes to the
        // same repo-relative tail too (task-6: cross-checkout reuse never
        // leaks a checkout path).
        assert_eq!(
            rebase("/writer/wt", "/reader/wt", "/reader/wt/src/a.rs"),
            "src/a.rs"
        );
        // An absolute path outside the writer root is left alone (no root to
        // strip).
        assert_eq!(
            rebase("/writer/wt", "/reader/wt", "/elsewhere/a.rs"),
            "/elsewhere/a.rs"
        );
    }

    #[test]
    fn cache_key_tracks_version_schema_projection_and_config() {
        let base = ScanConfigKey {
            languages: vec!["go".into()],
            excludes: vec![],
            modules: vec![],
            classification: classification_digest(None),
        };
        let k = CacheKey::compute(&base);
        // The binary version is folded in.
        assert_eq!(k.binary_version, env!("CARGO_PKG_VERSION"));
        // A config drift (an added exclude) changes the key.
        let mut cfg2 = base.clone();
        cfg2.excludes.push("vendor".into());
        let k2 = CacheKey::compute(&cfg2);
        assert!(!k.matches(&k2));
        // Ordering of the config lists never matters (sorted rendering).
        let mut cfg3 = base.clone();
        cfg3.languages.push("java".into());
        let mut cfg4 = ScanConfigKey {
            languages: vec!["java".into(), "go".into()],
            ..base.clone()
        };
        let _ = &mut cfg4;
        assert_eq!(
            CacheKey::compute(&cfg3).config,
            CacheKey::compute(&cfg4).config
        );
        // Identical configs render identical keys.
        assert!(CacheKey::compute(&base).matches(&CacheKey::compute(&base)));
    }

    /// phase-03 task-4 (Defect B): the loaded classification config
    /// (`apg/config.json`) is part of the global cache key. `classify_code_type`
    /// runs at ingest and only a FULL load reclassifies, so a classification
    /// change must force a full scan rather than silently reuse cached
    /// `code_type`s. Equal configs fold to equal keys and the digest is
    /// deterministic; a change to the `types` rules (or their order), the
    /// `default`, or the structural scope moves the key; and the existing
    /// language/exclude/module identity is unchanged.
    #[test]
    fn cache_key_folds_the_classification_config() {
        let a = ApgConfig {
            default: "src".into(),
            types: vec![code_type_rule("test", "**/tests/**")],
            structural: Some(structural_scope()),
        };
        let a_again = ApgConfig {
            default: "src".into(),
            types: vec![code_type_rule("test", "**/tests/**")],
            structural: Some(structural_scope()),
        };
        let key = |cfg: Option<&ApgConfig>| {
            CacheKey::compute(&ScanConfigKey {
                languages: vec!["go".into()],
                excludes: vec![],
                modules: vec![],
                classification: classification_digest(cfg),
            })
        };

        // (a) Equal configs → the same key; the digest is deterministic, and an
        // absent config has its own distinct, stable identity.
        let ka = key(Some(&a));
        let digest_a = classification_digest(Some(&a));
        let digest_none = classification_digest(None);
        assert!(ka.matches(&key(Some(&a_again))));
        assert_eq!(classification_digest(Some(&a_again)), digest_a);
        assert_eq!(classification_digest(None), digest_none);
        assert_ne!(digest_a, digest_none);
        assert!(!ka.matches(&key(None)));

        // (b) Each graph-affecting field moves the key.
        // `types`: a different glob, a different rule name, a different `names`
        // list, and a different rule ORDER (first-match wins) each matter.
        let glob_changed = ApgConfig {
            default: "src".into(),
            types: vec![code_type_rule("test", "**/spec/**")],
            structural: Some(structural_scope()),
        };
        assert!(!ka.matches(&key(Some(&glob_changed))));
        let name_changed = ApgConfig {
            default: "src".into(),
            types: vec![code_type_rule("generated", "**/tests/**")],
            structural: Some(structural_scope()),
        };
        assert!(!ka.matches(&key(Some(&name_changed))));
        let names_changed = ApgConfig {
            default: "src".into(),
            types: vec![CodeTypeRule {
                names: vec!["Other*".into()],
                ..code_type_rule("test", "**/tests/**")
            }],
            structural: Some(structural_scope()),
        };
        assert!(!ka.matches(&key(Some(&names_changed))));
        let ordered = |first: (&str, &str), second: (&str, &str)| ApgConfig {
            default: "src".into(),
            types: vec![
                code_type_rule(first.0, first.1),
                code_type_rule(second.0, second.1),
            ],
            structural: Some(structural_scope()),
        };
        assert!(
            !key(Some(&ordered(
                ("test", "**/tests/**"),
                ("generated", "**/*.pb.go")
            )))
            .matches(&key(Some(&ordered(
                ("generated", "**/*.pb.go"),
                ("test", "**/tests/**")
            ))))
        );
        // `default`.
        let default_changed = ApgConfig {
            default: "lib".into(),
            types: vec![code_type_rule("test", "**/tests/**")],
            structural: Some(structural_scope()),
        };
        assert!(!ka.matches(&key(Some(&default_changed))));
        // Structural scope: include, exclude, code_type, and absence vs presence.
        let include_changed = ApgConfig {
            default: "src".into(),
            types: vec![code_type_rule("test", "**/tests/**")],
            structural: Some(StructuralScope {
                include: vec!["**/*.yaml".into()],
                ..structural_scope()
            }),
        };
        assert!(!ka.matches(&key(Some(&include_changed))));
        let exclude_changed = ApgConfig {
            default: "src".into(),
            types: vec![code_type_rule("test", "**/tests/**")],
            structural: Some(StructuralScope {
                exclude: vec!["third_party/**".into()],
                ..structural_scope()
            }),
        };
        assert!(!ka.matches(&key(Some(&exclude_changed))));
        let code_type_changed = ApgConfig {
            default: "src".into(),
            types: vec![code_type_rule("test", "**/tests/**")],
            structural: Some(StructuralScope {
                code_type: Some("cfg".into()),
                ..structural_scope()
            }),
        };
        assert!(!ka.matches(&key(Some(&code_type_changed))));
        let no_structural = ApgConfig {
            default: "src".into(),
            types: vec![code_type_rule("test", "**/tests/**")],
            structural: None,
        };
        assert!(!ka.matches(&key(Some(&no_structural))));

        // (c) The language/exclude/module identity still holds: each moves the
        // key on its own, independent of the classification digest, while an
        // identical config stays stable.
        let base = ScanConfigKey {
            languages: vec!["go".into()],
            excludes: vec![],
            modules: vec![],
            classification: classification_digest(Some(&a)),
        };
        let built = CacheKey::compute(&base);
        let mut lang = base.clone();
        lang.languages.push("java".into());
        assert!(!built.matches(&CacheKey::compute(&lang)));
        let mut excl = base.clone();
        excl.excludes.push("vendor".into());
        assert!(!built.matches(&CacheKey::compute(&excl)));
        let mut mods = base.clone();
        mods.modules.push("core".into());
        assert!(!built.matches(&CacheKey::compute(&mods)));
        assert!(built.matches(&CacheKey::compute(&base)));
    }

    #[test]
    fn manifest_diff_reports_added_modified_removed() {
        let mut older = Manifest::default();
        older.entries.insert("a.go".into(), "oid-a".into());
        older.entries.insert("b.go".into(), "oid-b".into());
        let mut newer = Manifest::default();
        newer.entries.insert("a.go".into(), "oid-a".into()); // unchanged
        newer.entries.insert("b.go".into(), "oid-b2".into()); // modified
        newer.entries.insert("c.go".into(), "oid-c".into()); // added
        let d = older.diff(&newer);
        assert!(d.added.contains("c.go"));
        assert!(d.modified.contains("b.go"));
        assert!(d.removed.is_empty());
        assert_eq!(d.changed().len(), 2);
        // A deletion is reported as removed.
        let d2 = newer.diff(&older);
        assert!(d2.removed.contains("c.go"));
        assert!(d2.modified.contains("b.go"));
        assert!(d2.added.is_empty());
    }

    /// phase-04 task-20: the single-pass per-file index is falsifiably ONE
    /// traversal (its visit counters equal the graph's own sizes and do not
    /// grow as fragments are derived) and byte-identical to the unchanged
    /// pre-fix `from_graph` per-file scan oracle.
    #[test]
    fn file_index_visits_each_node_and_edge_once_and_matches_reference_fragments() {
        let g = index_fixture();
        let index = FileIndex::build(&g);
        let edge_total = g.contains.len()
            + g.calls.len()
            + g.uses.len()
            + g.unresolved_calls.len()
            + g.unresolved_uses.len();

        // (a) ONE traversal: the counters equal the graph's own sizes.
        assert_eq!(index.node_visits(), g.nodes.len());
        assert_eq!(index.edge_visits(), edge_total);

        // (b) equivalence for EVERY indexed file, against the unchanged
        // pre-fix `from_graph` oracle.
        let files = index.files();
        assert!(files.contains("/r/a.go"), "{files:?}");
        assert!(files.contains("/r/b.go"), "{files:?}");
        assert!(files.contains("/r/empty.go"), "{files:?}");
        let mut with_declarations = 0usize;
        for abs in &files {
            let reference = FileFragment::from_graph(&g, abs, "rel", "oid", "go");
            let got = FileFragment::from_index(&index, abs, "rel", "oid", "go");
            assert_eq!(got, reference, "index fragment differs for {abs}");
            if !got.nodes.is_empty() {
                with_declarations += 1;
            }
        }
        assert!(
            with_declarations >= 3,
            "the fixture must exercise real fragments"
        );

        // Deriving every fragment did NOT re-traverse the graph — the
        // counters stay frozen at their build-time values.
        assert_eq!(index.node_visits(), g.nodes.len());
        assert_eq!(index.edge_visits(), edge_total);

        // The unresolved-carrying fragment keeps its carried target rows, and
        // two edges to the same target dedup to ONE row within the fragment.
        let a = FileFragment::from_index(&index, "/r/a.go", "a.go", "oid-a", "go");
        let unresolved: Vec<&str> = a
            .nodes
            .iter()
            .filter(|n| n.kind == "unresolved")
            .map(|n| n.fqn.as_str())
            .collect();
        assert!(unresolved.contains(&"fmt.Println"), "{unresolved:?}");
        assert!(unresolved.contains(&"os.File"), "{unresolved:?}");
        assert_eq!(
            unresolved.iter().filter(|f| **f == "fmt.Println").count(),
            1,
            "within-fragment dedup: {unresolved:?}"
        );
        assert_eq!(a.modules, vec!["m".to_string()]);
        // Two files referencing the SAME unresolved target each carry their
        // own row; neither leaks the other's declarations.
        let b = FileFragment::from_index(&index, "/r/b.go", "b.go", "oid-b", "go");
        assert!(b.nodes.iter().any(|n| n.fqn == "fmt.Println"));
        assert!(
            !b.nodes.iter().any(|n| n.fqn == "m.a.A"),
            "no cross-file coupling"
        );
        assert!(
            !a.nodes.iter().any(|n| n.fqn == "m.b.B"),
            "no cross-file coupling"
        );

        // (c) a path the graph does not carry yields the empty fragment, and a
        // Module/Contains-only graph visits its objects but keys no file.
        let missing = FileFragment::from_index(&index, "/r/nope.go", "nope.go", "oid", "go");
        assert_eq!(
            missing,
            FileFragment::from_graph(&g, "/r/nope.go", "nope.go", "oid", "go")
        );
        assert!(missing.nodes.is_empty() && missing.edges.is_empty() && missing.modules.is_empty());

        let mut scaffold = Graph::default();
        scaffold.nodes.insert("m".into(), module_node());
        scaffold.nodes.insert("m.sub".into(), module_node());
        scaffold.contains.insert(("m".into(), "m.sub".into()));
        let sidx = FileIndex::build(&scaffold);
        assert_eq!(sidx.node_visits(), 2);
        assert_eq!(sidx.edge_visits(), 1);
        let sfrag = FileFragment::from_index(&sidx, "/r/x.go", "x.go", "oid", "go");
        assert_eq!(
            sfrag,
            FileFragment::from_graph(&scaffold, "/r/x.go", "x.go", "oid", "go")
        );
        assert!(sfrag.nodes.is_empty() && sfrag.edges.is_empty() && sfrag.modules.is_empty());
    }
}
