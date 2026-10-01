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
}
