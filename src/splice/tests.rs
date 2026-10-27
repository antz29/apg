use super::*;

/// int tier -- pure in-memory: the seed-vs-full-load DECISION surface wired
/// through the extracted equivalence predicate; no filesystem, database, git
/// or process. The `db.lbug` copy/schema/DB-equality half stays e2e below.
mod int {
    use super::*;

    /// fix-module-identity phase-06 task-10: the seed guard's content-key
    /// equivalence (`seed_key_matches`) is the same rule `seed_checked`
    /// applies — equal keys both present is the ONLY eligible combination.
    #[test]
    fn stale_seed_content_key_equivalence_is_wired() {
        assert!(seed_key_matches(Some("k1"), Some("k1")));
        // A missing key on EITHER side is never equivalent.
        assert!(!seed_key_matches(None, Some("k1")));
        assert!(!seed_key_matches(Some("k1"), None));
        assert!(!seed_key_matches(None, None));
        // A drift is ineligible.
        assert!(!seed_key_matches(Some("k1"), Some("k2")));
    }

    /// Every `SeedFallback` variant describes itself for the scan log (the
    /// dispatch seam's one-line message), each naming the full-load outcome.
    #[test]
    fn every_seed_fallback_variant_describes_itself() {
        let cases = [
            (
                SeedFallback::MissingPrevious,
                "no previous db.lbug to seed from",
            ),
            (SeedFallback::Unreadable("corrupt".into()), "corrupt"),
            (
                SeedFallback::IncompatibleSchema("missing tables: X".into()),
                "missing tables: X",
            ),
            (
                SeedFallback::StaleSeed {
                    seed: "seed-key".into(),
                    recorded: "rec-key".into(),
                },
                "seed-key",
            ),
            (
                SeedFallback::UnrepresentedAuthored {
                    assembled: "assembled-key".into(),
                    seed: "seed-key".into(),
                },
                "assembled-key",
            ),
            (
                SeedFallback::SeededCopyUnreadable("bad copy".into()),
                "bad copy",
            ),
        ];
        for (fallback, needle) in &cases {
            let described = fallback.describe();
            assert!(
                described.contains(needle),
                "{fallback:?} describe() must carry {needle:?}: {described}"
            );
        }
        // The stale-seed line names BOTH keys so the log attributes the drift.
        let stale = SeedFallback::StaleSeed {
            seed: "seed-key".into(),
            recorded: "rec-key".into(),
        }
        .describe();
        assert!(
            stale.contains("seed-key") && stale.contains("rec-key"),
            "{stale}"
        );
        // The unrepresented-authored line names BOTH authored/transient
        // digests so the log attributes the divergence, names the full-load
        // outcome, and is distinct from the content-key drift `StaleSeed`
        // reports (which names neither of these keys).
        let unrepresented = SeedFallback::UnrepresentedAuthored {
            assembled: "assembled-key".into(),
            seed: "seed-key".into(),
        }
        .describe();
        assert!(
            unrepresented.contains("assembled-key") && unrepresented.contains("seed-key"),
            "{unrepresented}"
        );
        assert!(unrepresented.contains("full load"), "{unrepresented}");
    }
}

/// unit tier -- pure in-memory: the assembled authored/transient digest reads
/// only an in-memory `Graph`; no filesystem, database, git or process.
mod unit {
    use super::*;
    use crate::graph::{Graph, Node, NodeKind};

    /// The digest covers authored/transient node rows and the rows of edges
    /// those nodes author; it is order-independent and property-sensitive, and
    /// it ignores code rows/edges entirely.
    #[test]
    fn assembled_authored_identity_is_order_independent_property_sensitive_and_code_blind() {
        let note = |fqn: &str, body: &str| {
            (
                fqn.to_string(),
                Node {
                    kind: NodeKind::Note,
                    body: Some(body.to_string()),
                    ..Default::default()
                },
            )
        };
        let module = |fqn: &str| {
            (
                fqn.to_string(),
                Node {
                    kind: NodeKind::Module,
                    ..Default::default()
                },
            )
        };

        let (f1, n1) = note("requirements.note.n1", "one");
        let (f2, n2) = note("requirements.note.n2", "two");

        let mut a = Graph::default();
        a.nodes.insert(f1.clone(), n1.clone());
        a.nodes.insert(f2.clone(), n2.clone());
        a.details.insert((f1.clone(), f2.clone()));
        let digest = assembled_authored_identity(&a);

        // The same logical rows, inserted in the opposite order, hash the same:
        // the canonical form sorts before folding.
        let mut b = Graph::default();
        b.nodes.insert(f2.clone(), n2);
        b.nodes.insert(f1.clone(), n1);
        b.details.insert((f1.clone(), f2.clone()));
        assert_eq!(
            digest,
            assembled_authored_identity(&b),
            "insertion order must not change the digest"
        );

        // A body edit moves the digest: the seed DB does not carry this row
        // verbatim, so the splice is ineligible.
        b.nodes.get_mut("requirements.note.n1").unwrap().body = Some("changed".into());
        assert_ne!(
            digest,
            assembled_authored_identity(&b),
            "an authored property change must change the digest"
        );

        // Code nodes and code edges are invisible to the authored digest.
        let mut c = Graph::default();
        let (m, node_m) = module("rust.apg");
        let (f, node_f) = module("src/main.rs");
        c.nodes.insert(m.clone(), node_m);
        c.nodes.insert(f.clone(), node_f);
        c.contains.insert((m, f));
        assert_eq!(
            assembled_authored_identity(&Graph::default()),
            assembled_authored_identity(&c),
            "code rows must not affect the authored digest"
        );

        // An authored edge targeting a CODE FQN IS part of the digest.
        let mut d = Graph::default();
        let (n, node_n) = note("implementation.note.n1", "why");
        let (code, node_code) = module("rust.apg");
        d.nodes.insert(n.clone(), node_n);
        d.nodes.insert(code.clone(), node_code);
        let without = assembled_authored_identity(&d);
        d.details.insert((n, code));
        assert_ne!(
            without,
            assembled_authored_identity(&d),
            "a Details edge from an authored node must be in the digest"
        );
    }

    /// A raw authored edge whose `(table, from, to)` kind pair the DB schema
    /// does not declare — a legacy `Note`→`Note` `details`, the pair the
    /// write-time validator now refuses — is not representable in `db.lbug`:
    /// the loader (`load::tables::build_load_files`) writes only declared
    /// pairs, so the DB-side twin (`seed_authored_identity`) can never observe
    /// it. The graph-side canonical form must apply the SAME filter, or the two
    /// digests can never agree and `git::is_fresh` fails closed forever (the
    /// `project start` wedge this regresses).
    #[test]
    fn assembled_authored_identity_ignores_undeclared_pairs() {
        let note = |fqn: &str| {
            (
                fqn.to_string(),
                Node {
                    kind: NodeKind::Note,
                    ..Default::default()
                },
            )
        };
        let (a, na) = note("global.note.a");
        let (b, nb) = note("global.note.b");
        let mut g = Graph::default();
        g.nodes.insert(a.clone(), na);
        g.nodes.insert(b.clone(), nb);
        let without = assembled_authored_identity(&g);
        // Note→Note `details` is undeclared in the schema → DB-unrepresentable,
        // so it must not move the digest.
        g.details.insert((a.clone(), b));
        assert_eq!(
            without,
            assembled_authored_identity(&g),
            "an undeclared Note→Note details edge must not contribute to the digest"
        );
        // A DECLARED pair (Note→Module) still does.
        let (code, node_code) = (
            "rust.apg".to_string(),
            Node {
                kind: NodeKind::Module,
                ..Default::default()
            },
        );
        g.nodes.insert(code.clone(), node_code);
        let with_declared_target = assembled_authored_identity(&g);
        g.details.insert((a, code));
        assert_ne!(
            with_declared_target,
            assembled_authored_identity(&g),
            "a declared Note→Module details edge must contribute to the digest"
        );
    }

    /// The splice writer's planned-placeholder backstop covers EVERY
    /// status-bearing kind, not only the Module arm: a `planned` row can never
    /// re-mark a realized (non-`planned`) Struct, Function or File, while a
    /// realized row still writes its own cleared status and location. Selection
    /// only ever hands this writer an assembled-REAL row — the Phase-7 seed-row
    /// heal selects a planned seed placeholder by FQN only when the assembled
    /// graph holds the real declaration — so the writer's own guarantee is what
    /// pins the invariant against a future selection change.
    #[test]
    fn every_status_bearing_kind_keeps_a_realized_status_against_a_planned_row() {
        use crate::splice::apply::upsert_statement;

        let rows = vec!["{fqn: 'x', status: 'planned'}".to_string()];
        for label in ["Module", "Struct", "Function", "File"] {
            let stmt = upsert_statement(label, &rows).expect("a status-bearing label upserts");
            assert!(
                stmt.contains("ON CREATE SET") && stmt.contains("n.status = row.status"),
                "{label} must write the row's own status on create: {stmt}"
            );
            assert!(
                stmt.contains("ON MATCH SET")
                    && stmt.contains("row.status = 'planned'")
                    && stmt.contains("n.status <> 'planned'")
                    && stmt.contains("THEN n.status ELSE row.status END"),
                "{label} must keep a realized status against a planned row: {stmt}"
            );
        }
        // The located kinds still write the location of a realized row.
        for label in ["Struct", "Function"] {
            let stmt = upsert_statement(label, &rows).unwrap();
            for prop in [
                "n.path = row.path",
                "n.start = row.start",
                "n.`end` = row.end_pos",
                "n.start_line = row.sline",
                "n.end_line = row.eline",
            ] {
                assert!(stmt.contains(prop), "{label} must write {prop}: {stmt}");
            }
        }
        let file = upsert_statement("File", &rows).unwrap();
        for prop in ["n.start_line = row.sline", "n.end_line = row.eline"] {
            assert!(file.contains(prop), "File must write {prop}: {file}");
        }
        // Labels without a status column carry no status write at all.
        for label in ["Agent", "UnresolvedTarget", "Language"] {
            let stmt = upsert_statement(label, &rows).unwrap();
            assert!(
                !stmt.contains("n.status"),
                "{label} has no status column: {stmt}"
            );
        }
        // No rows (or an unknown label) is not an upsert.
        assert!(upsert_statement("Module", &[]).is_none());
        assert!(upsert_statement("Task", &rows).is_none());
    }
}
