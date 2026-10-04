use super::*;
use crate::schema::Record;
use crate::testutil::{code_universe, in_edge, node, out_edge};

/// unit tier -- pure in-memory: no filesystem, database, git or process.
mod unit {
    use super::*;

    /// The catalog is the SPEC §3.1 table: per-layer dirs and node types,
    /// verbatim (global holds constraint before note; implementation holds
    /// only the attach-only pair).
    #[test]
    fn catalog_matches_the_spec_table() {
        let rows: Vec<(&str, &[&str])> = Layer::ALL
            .into_iter()
            .map(|l| (l.layer_dir(), l.node_types()))
            .collect();
        assert_eq!(
            rows,
            vec![
                (
                    "requirements",
                    &["stakeholder", "user", "requirement", "note", "constraint"][..]
                ),
                (
                    "domain",
                    &["group", "entity", "value", "service", "note", "constraint"][..]
                ),
                (
                    "solution",
                    &[
                        "system",
                        "container",
                        "component",
                        "person",
                        "note",
                        "constraint"
                    ][..]
                ),
                (
                    "plans",
                    &["plan-phase", "task", "module", "file", "struct", "function"][..]
                ),
                ("implementation", &["note", "constraint"][..]),
                ("global", &["constraint", "note"][..]),
            ]
        );
    }

    /// Storage policy (SPEC §3.1): the four file-backed layers are durable,
    /// plans is the only transient layer (`.trans/plans/` only, per branch),
    /// implementation is scanned code.
    #[test]
    fn storage_policy_marks_only_plans_transient() {
        for l in Layer::ALL {
            match l.storage() {
                StoragePolicy::Durable => assert!(
                    matches!(
                        l,
                        Layer::Requirements | Layer::Domain | Layer::Solution | Layer::Global
                    ),
                    "{l:?} must not be durable"
                ),
                StoragePolicy::TransientPlans => {
                    assert_eq!(l, Layer::Plans, "only plans is transient")
                }
                StoragePolicy::ScannedCode => {
                    assert_eq!(
                        l,
                        Layer::Implementation,
                        "only implementation is scanned code"
                    )
                }
            }
        }
    }

    /// Implementation holds exactly the attach-only types; global holds the
    /// laws (constraint) and the notes on them.
    #[test]
    fn implementation_is_attach_only() {
        assert_eq!(Layer::Implementation.storage(), StoragePolicy::ScannedCode);
        assert_eq!(Layer::Implementation.node_types(), &["note", "constraint"]);
        assert_eq!(Layer::Global.node_types(), &["constraint", "note"]);
    }

    /// The durable tree constant is exactly the catalog projection over the
    /// non-transient layers (plans has no durable dir; implementation's
    /// attach-only dirs are durable) — the durable dir list matches the
    /// layout constants, in layout order.
    #[test]
    fn layers_tree_rows_match_the_catalog() {
        let durable: Vec<(&str, &[&str])> = Layer::ALL
            .into_iter()
            .filter(|l| l.storage() != StoragePolicy::TransientPlans)
            .map(|l| (l.layer_dir(), l.node_types()))
            .collect();
        assert_eq!(LAYERS_TREE, durable.as_slice());
    }

    /// The `.trans` mirrors are complete: all six tiers in the §4.1 diagram
    /// order — plans first, then the five feedback mirrors incl. global —
    /// with unique dirs.
    #[test]
    fn trans_mirrors_cover_all_six_layers() {
        assert_eq!(
            TRANS_MIRRORS,
            [
                Layer::Plans,
                Layer::Requirements,
                Layer::Domain,
                Layer::Solution,
                Layer::Implementation,
                Layer::Global,
            ]
        );
        let dirs: Vec<&str> = TRANS_MIRRORS.iter().map(|l| l.layer_dir()).collect();
        let mut unique = dirs.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), dirs.len(), "mirror tier dirs must be unique");
    }

    /// The SPEC §3.3 name allowlist `[a-z0-9][a-z0-9-]*`: lowercase letters,
    /// digits, and hyphens (not first); a leading digit and a trailing hyphen
    /// are fine. Refuse — never sanitize.
    #[test]
    fn name_allowlist_refuses_invalid_names() {
        let empty = BTreeSet::new();
        let props = BTreeMap::new();
        for name in [
            "customer",
            "a",
            "0",
            "order-line",
            "a-b-c",
            "123",
            "line-",
            "x0",
        ] {
            assert!(
                validate_node(Layer::Domain, "value", name, &props, &empty).is_ok(),
                "{name} must pass the allowlist"
            );
        }
        for name in [
            "CamelCase",
            "with.dot",
            "with space",
            "with_underscore",
            "-lead",
            "",
            "UPPER",
        ] {
            assert!(
                validate_node(Layer::Domain, "value", name, &props, &empty).is_err(),
                "{name} must be refused (never sanitized)"
            );
        }
    }

    /// Type must exist in its layer (SPEC §3.3) — case-sensitive exact match
    /// against the catalog's lowercase type spellings. The cryptic DDD names
    /// are not types (SPEC §3.1 collapses them into Group).
    #[test]
    fn type_must_exist_in_its_layer() {
        let empty = BTreeSet::new();
        let props = BTreeMap::new();
        for bad in [
            "aggregate",
            "bounded-context",
            "subdomain",
            "domain-rule",
            "domain-process",
        ] {
            let err = validate_node(Layer::Domain, bad, "x", &props, &empty).unwrap_err();
            assert!(
                err.to_string().contains("does not exist in layer"),
                "{bad}: {err}"
            );
        }
        // Case-sensitive: the catalog spells types lowercase.
        assert!(validate_node(Layer::Domain, "Entity", "x", &props, &empty).is_err());
        // A type of another layer is refused here.
        assert!(validate_node(Layer::Domain, "stakeholder", "x", &props, &empty).is_err());
        // The exact lowercase spelling passes the type check; the Entity kind
        // rule is then what fires.
        let err = validate_node(Layer::Domain, "entity", "customer", &props, &empty).unwrap_err();
        assert!(err.to_string().contains("requires"), "{err}");
    }

    /// Entity requires `kind` ∈ {entity, event} — the spec's only "requires".
    #[test]
    fn entity_kind_is_required_and_validated() {
        let empty = BTreeSet::new();
        let err = validate_node(
            Layer::Domain,
            "entity",
            "customer",
            &BTreeMap::new(),
            &empty,
        )
        .unwrap_err();
        assert!(err.to_string().contains("requires"), "{err}");
        for kind in ["entity", "event"] {
            let props = BTreeMap::from([("kind".to_string(), kind.to_string())]);
            assert!(
                validate_node(Layer::Domain, "entity", "customer", &props, &empty).is_ok(),
                "kind {kind} must be accepted"
            );
        }
        let props = BTreeMap::from([("kind".to_string(), "thing".to_string())]);
        let err = validate_node(Layer::Domain, "entity", "customer", &props, &empty).unwrap_err();
        assert!(err.to_string().contains("entity|event"), "{err}");
    }

    /// Group: `attribute` ∈ {core, supporting, generic} and `root` are both
    /// optional; when present they are validated — root against the name
    /// allowlist, independent of the attribute.
    #[test]
    fn group_attribute_and_root_optional_but_validated() {
        let empty = BTreeSet::new();
        // No properties at all — valid.
        assert!(validate_node(Layer::Domain, "group", "sales", &BTreeMap::new(), &empty).is_ok());
        for attr in ["core", "supporting", "generic"] {
            let props = BTreeMap::from([("attribute".to_string(), attr.to_string())]);
            assert!(
                validate_node(Layer::Domain, "group", "sales", &props, &empty).is_ok(),
                "attribute {attr} must be accepted"
            );
        }
        let props = BTreeMap::from([("attribute".to_string(), "dubious".to_string())]);
        let err = validate_node(Layer::Domain, "group", "sales", &props, &empty).unwrap_err();
        assert!(err.to_string().contains("core|supporting|generic"), "{err}");
        // root is independent of attribute and format-validated.
        let props = BTreeMap::from([("root".to_string(), "sales-root".to_string())]);
        assert!(validate_node(Layer::Domain, "group", "sales", &props, &empty).is_ok());
        let props = BTreeMap::from([("root".to_string(), "SalesRoot".to_string())]);
        let err = validate_node(Layer::Domain, "group", "sales", &props, &empty).unwrap_err();
        assert!(err.to_string().contains("allowlist"), "{err}");
    }

    /// Container `kind` ∈ {app, service, db, queue} — validated when present,
    /// optional otherwise (SPEC §3.1 "takes").
    #[test]
    fn container_kind_validated_when_present() {
        let empty = BTreeSet::new();
        assert!(
            validate_node(
                Layer::Solution,
                "container",
                "api",
                &BTreeMap::new(),
                &empty
            )
            .is_ok()
        );
        for kind in ["app", "service", "db", "queue"] {
            let props = BTreeMap::from([("kind".to_string(), kind.to_string())]);
            assert!(
                validate_node(Layer::Solution, "container", "api", &props, &empty).is_ok(),
                "kind {kind} must be accepted"
            );
        }
        let props = BTreeMap::from([("kind".to_string(), "web".to_string())]);
        let err = validate_node(Layer::Solution, "container", "api", &props, &empty).unwrap_err();
        assert!(err.to_string().contains("app|service|db|queue"), "{err}");
    }

    /// Unknown property keys are metadata — allowed, never identity (SPEC
    /// §4.1: short ids "may exist as metadata only"); a known key on a type
    /// that does not take it is left alone too.
    #[test]
    fn unknown_metadata_keys_are_allowed() {
        let empty = BTreeSet::new();
        let props = BTreeMap::from([
            ("id".to_string(), "R1".to_string()),
            ("kind".to_string(), "thing".to_string()),
        ]);
        assert!(validate_node(Layer::Requirements, "requirement", "login", &props, &empty).is_ok());
    }

    /// Names are unique per (layer, type) — the same name in two different
    /// types (or layers) is fine; a duplicate in the same (layer, type) is
    /// refused.
    #[test]
    fn names_unique_per_layer_and_type() {
        let existing: BTreeSet<(Layer, String, String)> =
            BTreeSet::from([(Layer::Domain, "entity".to_string(), "customer".to_string())]);
        let props = BTreeMap::from([("kind".to_string(), "entity".to_string())]);
        // Same name, different type in the same layer — fine.
        assert!(validate_node(Layer::Domain, "value", "customer", &props, &existing).is_ok());
        // Same name, different layer — fine.
        assert!(validate_node(Layer::Solution, "component", "customer", &props, &existing).is_ok());
        // Same (layer, type, name) — refused.
        let err =
            validate_node(Layer::Domain, "entity", "customer", &props, &existing).unwrap_err();
        assert!(err.to_string().contains("duplicate"), "{err}");
    }

    /// contains/depends-on trees must be acyclic (SPEC §3.3). The helper
    /// validates the per-change edge set; every endpoint must resolve into
    /// the change universe (existing nodes plus co-proposed nodes).
    #[test]
    fn tree_acyclicity_refuses_contains_and_depends_on_cycles() {
        let universe: BTreeSet<(Layer, String, String)> = BTreeSet::from([
            (
                Layer::Requirements,
                "requirement".to_string(),
                "r1".to_string(),
            ),
            (
                Layer::Requirements,
                "requirement".to_string(),
                "r2".to_string(),
            ),
            (
                Layer::Requirements,
                "requirement".to_string(),
                "r3".to_string(),
            ),
            (Layer::Domain, "group".to_string(), "g1".to_string()),
            (Layer::Domain, "group".to_string(), "g2".to_string()),
        ]);
        // A contains cycle (Group nesting) is refused.
        let contains_cycle: &[(&str, &str)] = &[
            ("domain.group.g1", "domain.group.g2"),
            ("domain.group.g2", "domain.group.g1"),
        ];
        let err = validate_trees_acyclic(contains_cycle, &universe).unwrap_err();
        assert!(err.to_string().contains("cycle"), "{err}");
        // A depends-on cycle (Requirement) is refused.
        let depends_cycle: &[(&str, &str)] = &[
            ("requirements.requirement.r1", "requirements.requirement.r2"),
            ("requirements.requirement.r2", "requirements.requirement.r3"),
            ("requirements.requirement.r3", "requirements.requirement.r1"),
        ];
        let err = validate_trees_acyclic(depends_cycle, &universe).unwrap_err();
        assert!(err.to_string().contains("cycle"), "{err}");
        // A self-loop is a cycle too ("a node cannot contain itself").
        let self_loop: &[(&str, &str)] =
            &[("requirements.requirement.r1", "requirements.requirement.r1")];
        assert!(validate_trees_acyclic(self_loop, &universe).is_err());
        // A DAG passes.
        let dag: &[(&str, &str)] = &[
            ("requirements.requirement.r1", "requirements.requirement.r2"),
            ("requirements.requirement.r2", "requirements.requirement.r3"),
        ];
        assert!(validate_trees_acyclic(dag, &universe).is_ok());
        // A dangling endpoint is refused (write-time error, SPEC §3.3).
        let dangling: &[(&str, &str)] = &[(
            "requirements.requirement.r1",
            "requirements.requirement.nope",
        )];
        let err = validate_trees_acyclic(dangling, &universe).unwrap_err();
        assert!(
            err.to_string().contains("not a node of the change"),
            "{err}"
        );
    }

    /// Every §3.3 edge kind accepts at least one valid (source, target) shape:
    /// each matrix row, both code-exempt kinds (`implemented-by` with a code
    /// FQN target, `details` from a note in every authoring layer to any
    /// target), and the `represents` pair.
    #[test]
    fn every_edge_kind_accepts_a_valid_shape() {
        let ok: &[(&str, &str, &str)] = &[
            (
                "contains",
                "requirements.stakeholder.s1",
                "requirements.requirement.r1",
            ),
            (
                "contains",
                "requirements.user.u1",
                "requirements.requirement.r1",
            ),
            (
                "contains",
                "requirements.requirement.r1",
                "requirements.requirement.r2",
            ),
            ("contains", "domain.group.g1", "domain.group.g2"),
            ("contains", "domain.group.g1", "domain.entity.e1"),
            ("contains", "domain.group.g1", "domain.value.v1"),
            ("contains", "domain.group.g1", "domain.service.svc1"),
            ("contains", "solution.system.sys1", "solution.container.c1"),
            (
                "contains",
                "solution.container.c1",
                "solution.component.cmp1",
            ),
            ("drives", "requirements.requirement.r1", "domain.group.g1"),
            ("drives", "requirements.requirement.r1", "domain.entity.e1"),
            ("drives", "requirements.requirement.r1", "domain.value.v1"),
            (
                "drives",
                "requirements.requirement.r1",
                "domain.service.svc1",
            ),
            ("realised-by", "domain.group.g1", "solution.system.sys1"),
            ("realised-by", "domain.entity.e1", "solution.container.c1"),
            (
                "realised-by",
                "domain.service.svc1",
                "solution.component.cmp1",
            ),
            (
                "implemented-by",
                "solution.system.sys1",
                "apg.artifacts.write_jsonl_and_reingest",
            ),
            (
                "implemented-by",
                "solution.container.c1",
                "apg.layers.validate_edges",
            ),
            ("implemented-by", "solution.component.cmp1", "apg.main"),
            ("calls", "domain.service.svc1", "domain.service.svc2"),
            ("publishes", "domain.service.svc1", "domain.entity.e1"),
            ("subscribes", "domain.service.svc1", "domain.entity.e1"),
            (
                "depends-on",
                "requirements.requirement.r1",
                "requirements.requirement.r2",
            ),
            ("uses", "solution.person.p1", "solution.system.sys1"),
            ("represents", "requirements.user.u1", "domain.entity.e1"),
            ("represents", "domain.entity.e1", "solution.person.p1"),
            (
                "details",
                "requirements.note.n1",
                "requirements.requirement.r1",
            ),
            ("details", "domain.note.n1", "domain.entity.e1"),
            ("details", "solution.note.n1", "solution.system.sys1"),
            (
                "details",
                "implementation.note.n1",
                "requirements.requirement.r1",
            ),
            ("details", "global.note.n1", "requirements.requirement.r1"),
        ];
        for &(kind, src, dst) in ok {
            assert!(
                validate_edge(kind, src, dst).is_ok(),
                "{kind} {src} -> {dst} must be accepted"
            );
        }
    }

    /// A rejected (kind, source, target) shape errors naming the kind, the
    /// source, and the target — tier skips and wrong-type shapes included.
    #[test]
    fn rejected_shapes_name_kind_source_and_target() {
        let bad: &[(&str, &str, &str)] = &[
            // calls from an Entity (not Service).
            ("calls", "domain.entity.e1", "domain.service.svc1"),
            // drives from a Group — a tier skip (Domain → Solution).
            ("drives", "domain.group.g1", "solution.system.sys1"),
            // contains from a Container to a Requirement — a tier skip.
            (
                "contains",
                "solution.container.c1",
                "requirements.requirement.r1",
            ),
            // depends-on from a Domain entity.
            (
                "depends-on",
                "domain.entity.e1",
                "requirements.requirement.r1",
            ),
            // realised-by from a Requirement — a tier skip.
            (
                "realised-by",
                "requirements.requirement.r1",
                "solution.system.sys1",
            ),
            // uses from a Service (must be Person).
            ("uses", "domain.service.svc1", "solution.system.sys1"),
            // publishes from a Group (must be Service).
            ("publishes", "domain.group.g1", "domain.entity.e1"),
            // subscribes to a Group (target must be an Entity).
            ("subscribes", "domain.service.svc1", "domain.group.g1"),
            // represents Entity → System (must be Entity → Person).
            ("represents", "domain.entity.e1", "solution.system.sys1"),
        ];
        for &(kind, src, dst) in bad {
            let msg = validate_edge(kind, src, dst).unwrap_err().to_string();
            assert!(msg.contains(kind), "must name kind: {msg}");
            assert!(msg.contains(src), "must name source: {msg}");
            assert!(msg.contains(dst), "must name target: {msg}");
        }
    }

    /// `implemented-by` accepts a code-FQN target (exempt — `validate_code_refs`
    /// owns it, task-10) and refuses any non-Solution source.
    #[test]
    fn implemented_by_accepts_code_fqn_target_and_refuses_non_solution_source() {
        for src in [
            "solution.system.payments",
            "solution.container.api",
            "solution.component.checkout",
        ] {
            assert!(
                validate_edge(
                    "implemented-by",
                    src,
                    "apg.artifacts.write_jsonl_and_reingest"
                )
                .is_ok(),
                "{src} must be a valid implemented-by source"
            );
        }
        for src in [
            "domain.service.checkout",
            "requirements.requirement.r1",
            "domain.entity.customer",
            "solution.person.p1",
        ] {
            let msg = validate_edge("implemented-by", src, "apg.main")
                .unwrap_err()
                .to_string();
            assert!(msg.contains("implemented-by"), "{msg}");
            assert!(msg.contains(src), "{msg}");
        }
    }

    /// `details` accepts any target — authored OR code — except a `note`
    /// (a Note-to-Note pair has no durable edge), and enforces that the source
    /// is a `note` (spanning every authoring layer).
    #[test]
    fn details_refuses_note_target_and_enforces_note_source() {
        // Every other authored node target — any layer — plus a code FQN is
        // accepted.
        for target in [
            "requirements.stakeholder.sh1",
            "requirements.user.u1",
            "requirements.requirement.r1",
            "requirements.constraint.c1",
            "domain.group.g1",
            "domain.entity.e1",
            "domain.value.v1",
            "domain.service.svc1",
            "domain.constraint.c1",
            "solution.system.sys1",
            "solution.container.ct1",
            "solution.component.cp1",
            "solution.person.p1",
            "solution.constraint.c1",
            "implementation.constraint.c1",
            "global.constraint.c1",
            // A code FQN target is exempt (not parsed).
            "apg.artifacts.write_jsonl_and_reingest",
        ] {
            assert!(
                validate_edge("details", "requirements.note.n1", target).is_ok(),
                "details target {target} must be accepted"
            );
        }
        for src in [
            "requirements.note.n1",
            "domain.note.n1",
            "solution.note.n1",
            "implementation.note.n1",
            "global.note.n1",
        ] {
            assert!(
                validate_edge("details", src, "requirements.requirement.r1").is_ok(),
                "{src} must be a valid details source"
            );
        }
        // A `note` target in any layer is refused, naming the kind, source and
        // target.
        for target in [
            "requirements.note.n2",
            "domain.note.n2",
            "solution.note.n2",
            "implementation.note.n2",
            "global.note.n2",
        ] {
            let msg = validate_edge("details", "requirements.note.n1", target)
                .unwrap_err()
                .to_string();
            assert!(msg.contains("details"), "must name kind: {msg}");
            assert!(
                msg.contains("requirements.note.n1"),
                "must name source: {msg}"
            );
            assert!(msg.contains(target), "must name target: {msg}");
        }
        for src in [
            "requirements.requirement.r1",
            "domain.entity.e1",
            "solution.system.sys1",
        ] {
            let msg = validate_edge("details", src, "requirements.requirement.r1")
                .unwrap_err()
                .to_string();
            assert!(msg.contains("details"), "{msg}");
            assert!(msg.contains(src), "{msg}");
        }
    }

    /// A dangling authored endpoint — malformed FQN or unknown layer — is a
    /// write-time error for every matrix kind (SPEC §3.3). Code FQNs only pass
    /// through the exempt endpoints (`implemented-by`/`details` targets).
    #[test]
    fn dangling_authored_endpoint_is_a_write_time_error() {
        // Unknown layer in the source.
        let msg = validate_edge("drives", "banana.type.name", "domain.group.g1")
            .unwrap_err()
            .to_string();
        assert!(msg.contains("banana"), "{msg}");
        // Unknown layer in the target.
        let msg = validate_edge("drives", "requirements.requirement.r1", "banana.type.name")
            .unwrap_err()
            .to_string();
        assert!(msg.contains("banana"), "{msg}");
        // Malformed FQN (not <layer>.<type>.<name>) on a matrix-kind endpoint.
        let msg = validate_edge(
            "contains",
            "requirements.requirement",
            "requirements.requirement.r1",
        )
        .unwrap_err()
        .to_string();
        assert!(msg.contains("FQN"), "{msg}");
        // A code FQN in a non-exempt endpoint (drives target) is a dangling ref.
        let msg = validate_edge("drives", "requirements.requirement.r1", "apg.main")
            .unwrap_err()
            .to_string();
        assert!(msg.contains("apg.main"), "{msg}");
    }

    /// Anything outside the §3.3 durable list — §5 plan/feedback edges
    /// (`reviews`, `gates`, `satisfies`, task verbs) and gibberish — is refused
    /// as an unknown edge kind.
    #[test]
    fn unknown_edge_kind_is_refused() {
        for kind in [
            "reviews",
            "gates",
            "satisfies",
            "creates",
            "modifies",
            "deletes",
            "banana",
        ] {
            let msg = validate_edge(
                kind,
                "requirements.requirement.r1",
                "requirements.requirement.r2",
            )
            .unwrap_err()
            .to_string();
            assert!(msg.contains("unknown edge kind"), "{msg}");
            assert!(msg.contains(kind), "{msg}");
        }
    }

    /// `represents` has exactly the two matrix rows — User → Entity and Entity
    /// → Person; an Entity → System hop is refused (that would be a tier skip).
    #[test]
    fn represents_both_rows_and_refuses_tier_skips() {
        assert!(validate_edge("represents", "requirements.user.u1", "domain.entity.e1").is_ok());
        assert!(validate_edge("represents", "domain.entity.e1", "solution.person.p1").is_ok());
        let msg = validate_edge("represents", "domain.entity.e1", "solution.system.sys1")
            .unwrap_err()
            .to_string();
        assert!(msg.contains("represents"), "{msg}");
        assert!(msg.contains("domain.entity.e1"), "{msg}");
        assert!(msg.contains("solution.system.sys1"), "{msg}");
    }

    /// The batch entry point loops [`validate_edge`] over every edge; any bad
    /// edge in the set fails the whole batch.
    #[test]
    fn validate_edges_batch_loops_validate_edge() {
        let ok: &[(&str, &str, &str)] = &[
            (
                "contains",
                "requirements.requirement.r1",
                "requirements.requirement.r2",
            ),
            ("drives", "requirements.requirement.r1", "domain.group.g1"),
        ];
        assert!(validate_edges(ok).is_ok());
        let bad: &[(&str, &str, &str)] = &[
            (
                "contains",
                "requirements.requirement.r1",
                "requirements.requirement.r2",
            ),
            ("drives", "domain.group.g1", "solution.system.sys1"),
        ];
        let msg = validate_edges(bad).unwrap_err().to_string();
        assert!(msg.contains("drives"), "{msg}");
    }

    /// The plain-named domain types are accepted: the catalog's `group`,
    /// `entity`, `value`, `service` (SPEC §3.1 — the cryptic DDD type names
    /// collapse into these; their refusal is covered in
    /// [`type_must_exist_in_its_layer`]). A plain-named node validates
    /// end-to-end with its own type's kind/attribute rule applied.
    #[test]
    fn plain_domain_types_accepted_with_plain_names() {
        let empty = BTreeSet::new();
        // group/value/service need no properties and pass untouched.
        for (node_type, name) in [
            ("group", "order-fulfilment"),
            ("value", "money"),
            ("service", "checkout"),
        ] {
            assert!(
                validate_node(Layer::Domain, node_type, name, &BTreeMap::new(), &empty).is_ok(),
                "plain {node_type} `{name}` must be accepted"
            );
        }
        // Entity is accepted once its required kind is present.
        let props = BTreeMap::from([("kind".to_string(), "entity".to_string())]);
        assert!(validate_node(Layer::Domain, "entity", "customer", &props, &empty).is_ok());
    }

    /// A `requirement` node is the ONE requirement type at every depth (SPEC
    /// §3.1: theme/epic/story/feature are not types — everything is
    /// `requirement`); a four-level requirement tree validates acyclically,
    /// and `contains`/`depends-on` between requirement nodes are both
    /// matrix-legal at any depth.
    #[test]
    fn requirement_tree_at_all_depths_is_one_node_type() {
        let empty = BTreeSet::new();
        let props = BTreeMap::new();
        for pseudo in ["theme", "epic", "story", "feature"] {
            let err = validate_node(Layer::Requirements, pseudo, "x", &props, &empty).unwrap_err();
            assert!(
                err.to_string().contains("does not exist in layer"),
                "{pseudo}: {err}"
            );
        }
        let universe: BTreeSet<(Layer, String, String)> = ["r1", "r2", "r3", "r4"]
            .into_iter()
            .map(|n| {
                (
                    Layer::Requirements,
                    "requirement".to_string(),
                    n.to_string(),
                )
            })
            .collect();
        // Four levels of contains, plus a depends-on cross-link — still a DAG.
        let contains: &[(&str, &str)] = &[
            ("requirements.requirement.r1", "requirements.requirement.r2"),
            ("requirements.requirement.r2", "requirements.requirement.r3"),
            ("requirements.requirement.r3", "requirements.requirement.r4"),
        ];
        let depends: &[(&str, &str)] =
            &[("requirements.requirement.r1", "requirements.requirement.r4")];
        let all: Vec<(&str, &str)> = contains
            .iter()
            .copied()
            .chain(depends.iter().copied())
            .collect();
        assert!(
            validate_trees_acyclic(&all, &universe).is_ok(),
            "a four-level requirement tree must stay acyclic"
        );
        for &(src, dst) in contains {
            assert!(
                validate_edge("contains", src, dst).is_ok(),
                "contains {src} -> {dst} must be matrix-legal"
            );
        }
        for &(src, dst) in depends {
            assert!(
                validate_edge("depends-on", src, dst).is_ok(),
                "depends-on {src} -> {dst} must be matrix-legal"
            );
        }
    }

    /// The sequential spine (SPEC §3.2) is tier-locked by the matrix: the
    /// full legal chain Requirement --drives--> Domain --realised-by-->
    /// Solution --implemented-by--> code passes hop-by-hop, and a tier skip
    /// (Requirement directly realised-by a Solution — no Domain hop) is
    /// refused.
    #[test]
    fn sequential_spine_is_tier_locked() {
        // The full, legal spine from Requirement to code.
        assert!(validate_edge("drives", "requirements.requirement.r1", "domain.group.g1").is_ok());
        assert!(validate_edge("realised-by", "domain.group.g1", "solution.system.sys1").is_ok());
        assert!(validate_edge("implemented-by", "solution.system.sys1", "apg.main").is_ok());
        // A tier skip: Requirement realised-by Solution, skipping the Domain hop.
        let msg = validate_edge(
            "realised-by",
            "requirements.requirement.r1",
            "solution.system.sys1",
        )
        .unwrap_err()
        .to_string();
        assert!(msg.contains("realised-by"), "{msg}");
        assert!(msg.contains("requirements.requirement.r1"), "{msg}");
        assert!(msg.contains("solution.system.sys1"), "{msg}");
    }

    /// The §3.3 edge-kind matrix is complete and internally consistent: its
    /// rows cover exactly the two-authored-endpoint kinds (all of
    /// [`EDGE_KINDS`] except the code-exempt `implemented-by`/`details`), and
    /// every row's source/target type is a real type of its layer's catalog.
    #[test]
    fn edge_kind_matrix_is_complete_and_consistent() {
        let exempt = ["implemented-by", "details"];
        let kinds: BTreeSet<&str> = MATRIX.iter().map(|(k, ..)| *k).collect();
        let expected: BTreeSet<&str> = EDGE_KINDS
            .iter()
            .copied()
            .filter(|k| !exempt.contains(k))
            .collect();
        assert_eq!(
            kinds, expected,
            "matrix rows must cover exactly the non-exempt edge kinds"
        );
        for &(kind, sl, sts, tl, tts) in MATRIX {
            for t in sts {
                assert!(
                    sl.node_types().contains(t),
                    "{kind}: source type `{t}` not in layer {}",
                    sl.layer_dir()
                );
            }
            for t in tts {
                assert!(
                    tl.node_types().contains(t),
                    "{kind}: target type `{t}` not in layer {}",
                    tl.layer_dir()
                );
            }
        }
    }

    /// Two services that call each other are coupled: their owning groups are
    /// one derived pair — symmetric, nothing stored (SPEC §3.1).
    #[test]
    fn mutual_calls_couple_their_groups() {
        let edges: &[(&str, &str, &str)] = &[
            ("calls", "domain.service.orders", "domain.service.billing"),
            ("calls", "domain.service.billing", "domain.service.orders"),
        ];
        let owner = BTreeMap::from([
            (
                "domain.service.orders".to_string(),
                "domain.group.sales".to_string(),
            ),
            (
                "domain.service.billing".to_string(),
                "domain.group.billing".to_string(),
            ),
        ]);
        let coupled = derive_coupling(edges, &owner);
        assert_eq!(
            coupled,
            BTreeSet::from([(
                "domain.group.billing".to_string(),
                "domain.group.sales".to_string()
            )])
        );
    }

    /// A publisher and a subscriber sharing one event are coupled through the
    /// event medium; the event itself names no coupled group.
    #[test]
    fn publish_subscribe_via_shared_event_couples() {
        let edges: &[(&str, &str, &str)] = &[
            (
                "publishes",
                "domain.service.orders",
                "domain.entity.order-placed",
            ),
            (
                "subscribes",
                "domain.service.shipping",
                "domain.entity.order-placed",
            ),
        ];
        let owner = BTreeMap::from([
            (
                "domain.service.orders".to_string(),
                "domain.group.sales".to_string(),
            ),
            (
                "domain.service.shipping".to_string(),
                "domain.group.fulfilment".to_string(),
            ),
        ]);
        let coupled = derive_coupling(edges, &owner);
        assert_eq!(
            coupled,
            BTreeSet::from([(
                "domain.group.fulfilment".to_string(),
                "domain.group.sales".to_string()
            )])
        );
    }

    /// A calls chain A→B→C couples A and C — coupling is the transitive
    /// closure, every pair in the component.
    #[test]
    fn transitive_call_chain_couples_endpoints() {
        let edges: &[(&str, &str, &str)] = &[
            (
                "calls",
                "domain.service.checkout",
                "domain.service.inventory",
            ),
            (
                "calls",
                "domain.service.inventory",
                "domain.service.payments",
            ),
        ];
        let owner = BTreeMap::from([
            (
                "domain.service.checkout".to_string(),
                "domain.group.storefront".to_string(),
            ),
            (
                "domain.service.inventory".to_string(),
                "domain.group.stock".to_string(),
            ),
            (
                "domain.service.payments".to_string(),
                "domain.group.money".to_string(),
            ),
        ]);
        let coupled = derive_coupling(edges, &owner);
        // All three groups are pairwise coupled (3 unordered pairs).
        assert_eq!(
            coupled,
            BTreeSet::from([
                (
                    "domain.group.money".to_string(),
                    "domain.group.stock".to_string()
                ),
                (
                    "domain.group.money".to_string(),
                    "domain.group.storefront".to_string()
                ),
                (
                    "domain.group.stock".to_string(),
                    "domain.group.storefront".to_string()
                ),
            ])
        );
    }

    /// Two services with no connecting edge chain are NOT coupled —
    /// disconnected components yield no cross-component pair.
    #[test]
    fn unconnected_services_are_not_coupled() {
        let edges: &[(&str, &str, &str)] = &[
            ("calls", "domain.service.orders", "domain.service.billing"),
            ("calls", "domain.service.catalog", "domain.service.search"),
        ];
        let owner = BTreeMap::from([
            (
                "domain.service.orders".to_string(),
                "domain.group.sales".to_string(),
            ),
            (
                "domain.service.billing".to_string(),
                "domain.group.billing".to_string(),
            ),
            (
                "domain.service.catalog".to_string(),
                "domain.group.catalog".to_string(),
            ),
            (
                "domain.service.search".to_string(),
                "domain.group.search".to_string(),
            ),
        ]);
        let coupled = derive_coupling(edges, &owner);
        assert_eq!(
            coupled,
            BTreeSet::from([
                (
                    "domain.group.billing".to_string(),
                    "domain.group.sales".to_string()
                ),
                (
                    "domain.group.catalog".to_string(),
                    "domain.group.search".to_string()
                ),
            ])
        );
    }

    /// Coupling is DERIVED: the result is only group-FQN pairs — no edge
    /// kinds, no flavor values, no coupling node. The context-map flavor rides
    /// on the edge (an attribute the caller keeps), never in the derived
    /// structure.
    #[test]
    fn coupling_is_derived_and_flavor_is_not_structure() {
        let edges: &[(&str, &str, &str)] = &[
            ("calls", "domain.service.orders", "domain.service.billing"),
            (
                "publishes",
                "domain.service.orders",
                "domain.entity.order-placed",
            ),
            (
                "subscribes",
                "domain.service.shipping",
                "domain.entity.order-placed",
            ),
        ];
        let owner = BTreeMap::from([
            (
                "domain.service.orders".to_string(),
                "domain.group.sales".to_string(),
            ),
            (
                "domain.service.billing".to_string(),
                "domain.group.billing".to_string(),
            ),
            (
                "domain.service.shipping".to_string(),
                "domain.group.fulfilment".to_string(),
            ),
        ]);
        let coupled = derive_coupling(edges, &owner);
        // Three groups, all pairwise coupled through the call + the shared
        // event — but as DERIVED pairs, nothing stored.
        assert_eq!(coupled.len(), 3);
        for (a, b) in &coupled {
            // Every endpoint is a group FQN.
            assert!(a.starts_with("domain.group."), "{a}");
            assert!(b.starts_with("domain.group."), "{b}");
            // No flavor (edge attribute) leaks into the derived pair.
            for flavor in ["direct", "published", "translated", "shared", "coevolving"] {
                assert!(!a.contains(flavor), "flavor {flavor} leaked into {a}");
                assert!(!b.contains(flavor), "flavor {flavor} leaked into {b}");
            }
        }
    }

    /// Coupling is derived, never stored as a Group->Group edge: the §3.3
    /// matrix has no coupling row between groups — no coupling verb is an edge
    /// kind, the coupling edges (`calls`/`publishes`/`subscribes`) touch only
    /// Service/Entity, and the one Group->Group row (`contains`) is containment
    /// (nesting), not coupling.
    #[test]
    fn coupling_is_never_a_group_to_group_edge() {
        // No coupling verb is a durable edge kind — coupling is derived, not an
        // edge kind and not a stored artifact.
        for verb in ["couples", "coupled", "coupling", "coupled-to"] {
            assert!(
                !EDGE_KINDS.contains(&verb),
                "`{verb}` must not be an edge kind — coupling is derived, never stored"
            );
        }
        for &(kind, _, sts, _, tts) in MATRIX {
            let src_is_group = sts.contains(&"group");
            let dst_is_group = tts.contains(&"group");
            // The coupling edges never touch a Group endpoint (they are
            // Service->Service / Service->Entity).
            if matches!(kind, "calls" | "publishes" | "subscribes") {
                assert!(
                    !src_is_group && !dst_is_group,
                    "coupling edge `{kind}` must have no Group endpoint"
                );
            }
            // The only Group->Group row is `contains` (nesting).
            if src_is_group && dst_is_group {
                assert_eq!(
                    kind, "contains",
                    "Group->Group must be containment, never `{kind}`"
                );
            }
        }
    }

    /// The context-map flavor (direct/published/translated/shared/coevolving)
    /// is an EDGE attribute — never a node type, never an edge kind. A type
    /// named after a flavor is refused (it is not a catalog type); a flavor
    /// used as an edge kind is refused (unknown kind); the edge verb is
    /// `publishes`, not the flavor `published`.
    #[test]
    fn flavors_are_edge_attributes_never_types_or_kinds() {
        let empty = BTreeSet::new();
        let props = BTreeMap::new();
        for flavor in ["direct", "published", "translated", "shared", "coevolving"] {
            // Not a node type — validate_node refuses a type named after a flavor.
            let err = validate_node(Layer::Domain, flavor, "x", &props, &empty).unwrap_err();
            assert!(
                err.to_string().contains("does not exist in layer"),
                "flavor `{flavor}` as a node type: {err}"
            );
            // Not an edge kind — validate_edge refuses a flavor used as a kind.
            let err = validate_edge(flavor, "domain.service.a", "domain.service.b")
                .unwrap_err()
                .to_string();
            assert!(
                err.contains("unknown edge kind"),
                "flavor `{flavor}` as an edge kind: {err}"
            );
        }
        // The edge verb is `publishes`, not the flavor `published`.
        assert!(validate_edge("publishes", "domain.service.a", "domain.entity.e").is_ok());
    }

    /// A global constraint (layer Global) is well-formed on structure alone:
    /// its layer hosts the constraint type and its name is allowlisted, so
    /// `eval_constraint` passes it. Scope is declared by the layer — a global
    /// constraint binds the whole durable spec (R2 /
    /// global.constraint.spec-constraint-scope) — and satisfaction is
    /// review-only (R14): the binary reads neither the prose body nor any
    /// `attaches-to`.
    #[test]
    fn global_constraint_needs_no_attachment() {
        let empty = BTreeSet::new();
        assert!(eval_constraint(Layer::Global, "g1", &BTreeMap::new(), &empty).is_ok());
    }

    /// A local constraint (requirements/domain/solution) is well-formed on
    /// structure alone: its layer hosts the constraint type and its name is
    /// allowlisted. A present `attaches-to` property is IGNORED by
    /// `eval_constraint` — this function NAME is a legacy misnomer. Scope is
    /// declared by the layer (R2 / global.constraint.spec-constraint-scope) and
    /// satisfaction is review-only (R14).
    #[test]
    fn local_constraint_with_resolving_attachment_is_well_formed() {
        let existing: BTreeSet<(Layer, String, String)> = BTreeSet::from([
            (
                Layer::Requirements,
                "requirement".to_string(),
                "place-order".to_string(),
            ),
            (Layer::Domain, "entity".to_string(), "customer".to_string()),
            (
                Layer::Solution,
                "system".to_string(),
                "payments".to_string(),
            ),
        ]);
        for (layer, target) in [
            (
                Layer::Requirements,
                "requirements.requirement.place-order".to_string(),
            ),
            (Layer::Domain, "domain.entity.customer".to_string()),
            (Layer::Solution, "solution.system.payments".to_string()),
        ] {
            // The `attaches-to` value is present but irrelevant: the structure
            // check passes with or without it.
            let props = BTreeMap::from([(PROP_ATTACHES_TO.to_string(), target.clone())]);
            assert!(
                eval_constraint(layer, "law", &props, &existing).is_ok(),
                "{layer:?} as a constraint must validate on structure"
            );
        }
        // A local constraint without an attachment is equally fine.
        assert!(eval_constraint(Layer::Domain, "law", &BTreeMap::new(), &existing).is_ok());
    }

    /// A `constraint` type is refused in a layer that does not host constraints
    /// (plans); implementation hosts the attach-only pair, so it is NOT refused.
    #[test]
    fn constraint_type_refused_in_a_layer_without_constraints() {
        let empty = BTreeSet::new();
        let props = BTreeMap::new();
        let err = eval_constraint(Layer::Plans, "x", &props, &empty).unwrap_err();
        assert!(err.to_string().contains("does not exist in layer"), "{err}");
        // Implementation hosts note/constraint (attach-only) — a constraint
        // there is a real type.
        assert!(eval_constraint(Layer::Implementation, "x", &props, &empty).is_ok());
    }

    /// Satisfaction is NEVER evaluated: there is no constraint-expression
    /// language, and `eval_constraint` takes no prose input (the prose `body`
    /// is the node-file's top-level field, never read here) and ignores any
    /// `attaches-to`. A constraint whose prose "would fail" — contradictory, or
    /// even expression-looking — still passes structure validation regardless,
    /// because satisfaction is assessed by review only (R14).
    #[test]
    fn satisfaction_is_review_only_never_evaluated() {
        let existing: BTreeSet<(Layer, String, String)> =
            BTreeSet::from([(Layer::Domain, "entity".to_string(), "customer".to_string())]);
        // Contradictory prose (stands in for the top-level `body`) — the binary
        // never reads or evaluates it, and structure validation ignores it.
        let props = BTreeMap::from([
            (
                PROP_ATTACHES_TO.to_string(),
                "domain.entity.customer".to_string(),
            ),
            (
                "body".to_string(),
                "every order has a customer AND every order has no customer".to_string(),
            ),
        ]);
        assert!(eval_constraint(Layer::Domain, "law", &props, &existing).is_ok());
        // A global constraint with expression-looking prose is equally
        // unevaluated.
        let props = BTreeMap::from([(
            "body".to_string(),
            "count(entities) == 0 AND count(entities) > 0".to_string(),
        )]);
        assert!(eval_constraint(Layer::Global, "law", &props, &existing).is_ok());
    }

    /// The FQN builder is the inverse of [`parse_fqn`]: `<layer>.<type>.<name>`,
    /// no project prefix.
    #[test]
    fn fqn_builder_returns_layer_type_name() {
        assert_eq!(
            fqn(Layer::Requirements, "requirement", "place-order"),
            "requirements.requirement.place-order"
        );
        assert_eq!(
            fqn(Layer::Domain, "service", "checkout"),
            "domain.service.checkout"
        );
        assert_eq!(
            fqn(Layer::Global, "constraint", "law"),
            "global.constraint.law"
        );
        // And it is the inverse of parse_fqn.
        let (layer, node_type, name) = parse_fqn("domain.entity.customer").unwrap();
        assert_eq!(fqn(layer, &node_type, &name), "domain.entity.customer");
    }

    /// A fully paired set — A's out `contains -> B` and B's in `contains <-
    /// A`, matching source/kind/target/properties — passes.
    #[test]
    fn paired_in_and_out_edges_pass() {
        let mut a = node("requirements", "requirement", "a");
        a.out
            .push(out_edge("contains", "requirements.requirement.b"));
        let mut b = node("requirements", "requirement", "b");
        b.in_edges
            .push(in_edge("contains", "requirements.requirement.a"));
        assert!(check_edge_pairing(&[a, b]).is_ok());
    }

    /// An out edge in A without the matching in edge in B is an ERROR — the
    /// error names the node, the kind, and the missing counterpart.
    #[test]
    fn out_edge_without_matching_in_edge_errors() {
        let mut a = node("requirements", "requirement", "a");
        a.out
            .push(out_edge("contains", "requirements.requirement.b"));
        let b = node("requirements", "requirement", "b");
        let msg = check_edge_pairing(&[a, b]).unwrap_err().to_string();
        assert!(msg.contains("requirements.requirement.a"), "{msg}");
        assert!(msg.contains("contains"), "{msg}");
        assert!(msg.contains("BOTH endpoint files"), "{msg}");
    }

    /// An in edge in B without the matching out edge in A is an ERROR — the
    /// symmetric half of the pairwise rule.
    #[test]
    fn in_edge_without_matching_out_edge_errors() {
        let a = node("requirements", "requirement", "a");
        let mut b = node("requirements", "requirement", "b");
        b.in_edges
            .push(in_edge("contains", "requirements.requirement.a"));
        let msg = check_edge_pairing(&[a, b]).unwrap_err().to_string();
        assert!(msg.contains("requirements.requirement.b"), "{msg}");
        assert!(msg.contains("contains"), "{msg}");
        assert!(msg.contains("BOTH endpoint files"), "{msg}");
    }

    /// The SAME endpoints but DIFFERENT edge properties is a mismatch — a
    /// match requires identical properties, not merely endpoint existence.
    #[test]
    fn same_endpoints_different_properties_is_a_mismatch() {
        let mut a = node("requirements", "requirement", "a");
        let mut oe = out_edge("contains", "requirements.requirement.b");
        oe.properties
            .insert("flavor".to_string(), "direct".to_string());
        a.out.push(oe);
        let mut b = node("requirements", "requirement", "b");
        b.in_edges
            .push(in_edge("contains", "requirements.requirement.a"));
        let msg = check_edge_pairing(&[a, b]).unwrap_err().to_string();
        assert!(msg.contains("contains"), "{msg}");
        assert!(msg.contains("properties"), "{msg}");
    }

    /// A dangling authored target — parses as `<layer>.<type>.<name>` but no
    /// such node file exists — is an error, not a silent skip.
    #[test]
    fn dangling_authored_target_errors() {
        let mut a = node("requirements", "requirement", "a");
        a.out
            .push(out_edge("contains", "requirements.requirement.ghost"));
        let msg = check_edge_pairing(&[a]).unwrap_err().to_string();
        assert!(msg.contains("requirements.requirement.ghost"), "{msg}");
    }

    /// A dangling authored source on an in edge is the symmetric error.
    #[test]
    fn dangling_authored_source_errors() {
        let mut b = node("requirements", "requirement", "b");
        b.in_edges
            .push(in_edge("contains", "requirements.requirement.ghost"));
        let msg = check_edge_pairing(&[b]).unwrap_err().to_string();
        assert!(msg.contains("requirements.requirement.ghost"), "{msg}");
    }

    /// An `implemented-by` out edge to a code FQN (non-parsing target) is NOT
    /// flagged — code endpoints are exempt from the pairwise rule.
    #[test]
    fn implemented_by_to_code_fqn_is_not_flagged() {
        let mut sys = node("solution", "system", "payments");
        sys.out.push(out_edge("implemented-by", "apg.main"));
        assert!(check_edge_pairing(&[sys]).is_ok());
    }

    /// A `details` out edge to a code FQN is NOT flagged — the target is a
    /// code node (no file), so there is only the spec-side half.
    #[test]
    fn details_to_code_fqn_is_not_flagged() {
        let mut n = node("requirements", "note", "n1");
        n.out.push(out_edge(
            "details",
            "apg.artifacts.write_jsonl_and_reingest",
        ));
        assert!(check_edge_pairing(&[n]).is_ok());
    }

    /// A symmetric in edge whose source is a code FQN (non-parsing) is NOT
    /// flagged — the parse-based rule treats code endpoints as exempt in both
    /// directions.
    #[test]
    fn in_edge_from_code_fqn_is_not_flagged() {
        let mut sys = node("solution", "system", "payments");
        sys.in_edges.push(in_edge("implemented-by", "apg.main"));
        assert!(check_edge_pairing(&[sys]).is_ok());
    }

    /// The three-way split (SPEC §4.1): a scanned FQN is Real, a planned-only
    /// FQN is Pending, a FQN in neither universe is Drift.
    #[test]
    fn classify_code_ref_three_ways() {
        let scanned = code_universe(&["apg.layers.validate_edges", "apg.main"]);
        let planned = code_universe(&["apg.layers.ingest_tree"]);
        assert_eq!(
            classify_code_ref("apg.layers.validate_edges", &scanned, &planned),
            CodeRefStatus::Real
        );
        assert_eq!(
            classify_code_ref("apg.layers.ingest_tree", &scanned, &planned),
            CodeRefStatus::Pending
        );
        assert_eq!(
            classify_code_ref("apg.layers.gone", &scanned, &planned),
            CodeRefStatus::Drift
        );
    }

    /// The scanned graph is the stronger check: a FQN in BOTH scanned and
    /// planned is Real (it has landed), not Pending.
    #[test]
    fn scanned_wins_over_planned() {
        let scanned = code_universe(&["apg.main"]);
        let planned = code_universe(&["apg.main"]);
        assert_eq!(
            classify_code_ref("apg.main", &scanned, &planned),
            CodeRefStatus::Real
        );
    }

    /// The durable spec stays language-agnostic: an authored target resolves
    /// against a rooted scanned universe (child) and an un-rooted one
    /// (parent), so neither binary depends on the other. A target that is
    /// genuinely absent stays Drift.
    #[test]
    fn code_ref_resolution_tolerates_language_root() {
        let planned: BTreeSet<String> = BTreeSet::new();

        // Bare authored targets, rooted scanned universe.
        let rooted = code_universe(&[
            "rust.apg.main",
            "go.github.com/x/y.Store",
            "java.org.pkg.A",
            "ts.apg-tsfrontend.scanner.collectFile",
            "py.pkg.sub",
            "md./abs/docs",
        ]);
        assert_eq!(
            classify_code_ref("apg.main", &rooted, &planned),
            CodeRefStatus::Real
        );
        assert_eq!(
            classify_code_ref("github.com/x/y.Store", &rooted, &planned),
            CodeRefStatus::Real
        );
        assert_eq!(
            classify_code_ref("org.pkg.A", &rooted, &planned),
            CodeRefStatus::Real
        );
        assert_eq!(
            classify_code_ref("apg-tsfrontend.scanner.collectFile", &rooted, &planned),
            CodeRefStatus::Real
        );
        assert_eq!(
            classify_code_ref("pkg.sub", &rooted, &planned),
            CodeRefStatus::Real
        );
        assert_eq!(
            classify_code_ref("/abs/docs", &rooted, &planned),
            CodeRefStatus::Real
        );
        // A genuinely-absent target is still Drift.
        assert_eq!(
            classify_code_ref("apg.gone.Nope", &rooted, &planned),
            CodeRefStatus::Drift
        );
        assert!(
            validate_code_refs(&["apg.main", "github.com/x/y.Store"], &rooted, &planned).is_ok()
        );

        // Rooted authored target, un-rooted scanned universe (the parent).
        let bare = code_universe(&["apg.main"]);
        assert_eq!(
            classify_code_ref("rust.apg.main", &bare, &planned),
            CodeRefStatus::Real
        );
        assert_eq!(
            classify_code_ref("apg.main", &bare, &planned),
            CodeRefStatus::Real
        );

        // `js` is the unified JS/TS frontend's SECOND id — a JS-only repo
        // scans under `js` (not `ts`), and `available_languages` lists both —
        // so the tolerance must cover it.
        let js_rooted = code_universe(&["js.repo.calc.jsOuter"]);
        assert_eq!(
            classify_code_ref("repo.calc.jsOuter", &js_rooted, &planned),
            CodeRefStatus::Real
        );
        let js_bare = code_universe(&["repo.calc.jsOuter"]);
        assert_eq!(
            classify_code_ref("js.repo.calc.jsOuter", &js_bare, &planned),
            CodeRefStatus::Real
        );
    }

    /// `code_identity` strips exactly ONE leading language root — every root in
    /// `LANGUAGE_ROOTS`, a bare FQN unchanged, and only a real `<root>.`
    /// boundary (so `rusty.main` is never mangled).
    #[test]
    fn code_identity_strips_one_language_root() {
        for root in LANGUAGE_ROOTS {
            assert_eq!(
                code_identity(&format!("{root}.apg.cmd_scan")),
                "apg.cmd_scan",
                "`{root}.` must strip to the bare identity"
            );
        }
        assert_eq!(code_identity("apg.cmd_scan"), "apg.cmd_scan");
        assert_eq!(code_identity("/abs/store.go"), "/abs/store.go");
        // ONE root only, and only a real `<root>.` boundary.
        assert_eq!(code_identity("rust.rust.apg.main"), "rust.apg.main");
        assert_eq!(code_identity("rusty.main"), "rusty.main");
        assert_eq!(code_identity("rust"), "rust");
    }

    /// Pending is NOT an error — a planned-only FQN validates Ok (it realizes
    /// once the code lands), never a spec-drift bail.
    #[test]
    fn pending_code_ref_is_not_an_error() {
        let scanned = code_universe(&["apg.main"]);
        let planned = code_universe(&["apg.layers.ingest_tree"]);
        assert!(validate_code_refs(&["apg.layers.ingest_tree"], &scanned, &planned).is_ok());
    }

    /// A batch mixing Real and Pending refs validates Ok; a batch with one
    /// Drift bails naming the offending FQN.
    #[test]
    fn validate_code_refs_ok_on_real_and_pending_errors_on_drift() {
        let scanned = code_universe(&["apg.layers.validate_edges", "apg.main"]);
        let planned = code_universe(&["apg.layers.ingest_tree"]);
        // Real + Pending -> Ok.
        assert!(
            validate_code_refs(
                &["apg.layers.validate_edges", "apg.layers.ingest_tree"],
                &scanned,
                &planned
            )
            .is_ok()
        );
        // One Drift -> Err, naming the FQN.
        let err =
            validate_code_refs(&["apg.main", "apg.layers.gone"], &scanned, &planned).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("spec drift"), "{msg}");
        assert!(msg.contains("apg.layers.gone"), "{msg}");
    }

    /// Transient edges can never reach a committed durable node file: the §5
    /// plan/review/feedback kinds (`reviews`, `gates`, `satisfies`) live
    /// ENTIRELY under `.trans` (both halves there) — [`validate_edges`]
    /// (task-4) refuses them upstream of any durable write, so a durable node
    /// file whose `out` or `in` carries a transient kind is refused at
    /// validation and can never reach a committed file. The full `.trans`-side
    /// pairing (both halves in `.trans`) is phase-4/task-15 — NOT built here;
    /// this asserts only the durable-side refusal gate.
    #[test]
    fn durable_node_file_carrying_a_transient_edge_is_refused() {
        for kind in ["reviews", "gates", "satisfies"] {
            // A durable requirement file carrying the transient kind on an
            // out-edge — the edge is refused as an unknown durable kind.
            let mut a = node("requirements", "requirement", "a");
            a.out.push(out_edge(kind, "requirements.requirement.b"));
            let edges: Vec<(&str, &str, &str)> = a
                .out
                .iter()
                .map(|oe| {
                    (
                        oe.kind.as_str(),
                        "requirements.requirement.a",
                        oe.target.as_str(),
                    )
                })
                .collect();
            let msg = validate_edges(&edges).unwrap_err().to_string();
            assert!(msg.contains("unknown edge kind"), "{kind}: {msg}");
            assert!(msg.contains(kind), "{kind}: {msg}");

            // The same transient kind on an in-edge — equally refused.
            let mut a = node("requirements", "requirement", "a");
            a.in_edges.push(in_edge(kind, "requirements.requirement.b"));
            let edges: Vec<(&str, &str, &str)> = a
                .in_edges
                .iter()
                .map(|ie| {
                    (
                        ie.kind.as_str(),
                        ie.source.as_str(),
                        "requirements.requirement.a",
                    )
                })
                .collect();
            let msg = validate_edges(&edges).unwrap_err().to_string();
            assert!(msg.contains("unknown edge kind"), "{kind}: {msg}");
            assert!(msg.contains(kind), "{kind}: {msg}");
        }
    }

    /// [`LayersOverlay`]'s in-memory semantics (phase-00 task-1): a staged
    /// write resolves to the staged `NodeFile`, a staged delete marker
    /// resolves as absent, `apply_to_base` replaces staged writes / drops
    /// delete-marked identities / keeps unstaged base nodes, and
    /// `touched_fqns` names every staged identity. Pure in-memory — every
    /// asserted identity is staged, so the disk fallback is never reached and
    /// the root path is never read.
    #[test]
    fn overlay_resolves_staged_writes_and_deletes_in_memory() {
        // A deliberately non-existent root: staged identities resolve before
        // the disk fallback, so nothing under it is ever touched.
        let root = std::path::Path::new("/apg-overlay-test-never-reads-disk");

        let mut overlay = LayersOverlay::new();

        // A staged write that shadows a base node, and a staged write that
        // adds an identity absent from the base.
        let mut replaced = node("domain", "entity", "order");
        replaced.body = "staged".to_string();
        overlay
            .stage_write(replaced.clone())
            .expect("a known layer stages");
        let mut added = node("domain", "value", "money");
        added.body = "added".to_string();
        overlay
            .stage_write(added.clone())
            .expect("a known layer stages");

        // A delete marker for an identity the base carries.
        let stale = node("domain", "value", "stale");
        overlay.stage_delete(Layer::Domain, "value", "stale");

        // A staged write resolves to the staged content (not the base/disk).
        assert_eq!(
            overlay
                .read(root, Layer::Domain, "entity", "order")
                .unwrap(),
            Some(replaced.clone())
        );
        // A staged delete marker resolves as absent.
        assert_eq!(
            overlay.read(root, Layer::Domain, "value", "stale").unwrap(),
            None
        );

        // An explicit base: the shadowed node, the delete-marked node, and an
        // unstaged node that must survive untouched.
        let mut on_disk = node("domain", "entity", "order");
        on_disk.body = "base".to_string();
        let untouched = node("requirements", "requirement", "untouched");
        let base = vec![on_disk, stale, untouched.clone()];

        // The overlay replaces the staged write, drops the delete marker, and
        // keeps the unstaged node (base order preserved, additions appended).
        let effective = overlay.apply_to_base(&base);
        assert_eq!(effective, vec![replaced, untouched, added]);

        // Every staged identity — writes and delete markers — is named as its
        // `<layer>.<type>.<name>` FQN.
        assert_eq!(
            overlay.touched_fqns(),
            code_universe(&[
                "domain.entity.order",
                "domain.value.money",
                "domain.value.stale"
            ])
        );
    }

    /// [`LayersOverlay::over_base`] resolves a staged write to its buffered
    /// content over a caller-supplied base (phase-01 task-4): the staged
    /// `NodeFile` wins whether the base carries a differing node for the same
    /// identity or no node at all. Pure in-memory — no path is read.
    #[test]
    fn over_base_resolves_a_staged_write_to_its_buffered_content() {
        let mut overlay = LayersOverlay::new();
        let mut staged = node("domain", "entity", "order");
        staged.body = "staged".to_string();
        overlay
            .stage_write(staged.clone())
            .expect("a known layer stages");

        // A base with a *different* body for the same identity — the staged
        // content still wins.
        let mut from_base = node("domain", "entity", "order");
        from_base.body = "base".to_string();
        assert_eq!(
            overlay.over_base(&[from_base], Layer::Domain, "entity", "order"),
            Some(staged.clone())
        );

        // A base that does not carry the identity at all still yields the
        // staged write.
        assert_eq!(
            overlay.over_base(&[], Layer::Domain, "entity", "order"),
            Some(staged)
        );
    }

    /// [`LayersOverlay::over_base`] resolves a staged delete marker as absent
    /// even when the caller-supplied base carries the identity (phase-01
    /// task-4). Pure in-memory — no path is read.
    #[test]
    fn over_base_resolves_a_staged_delete_to_none() {
        let mut overlay = LayersOverlay::new();
        overlay.stage_delete(Layer::Domain, "value", "stale");

        let stale = node("domain", "value", "stale");
        assert_eq!(
            overlay.over_base(&[stale], Layer::Domain, "value", "stale"),
            None
        );
        assert_eq!(
            overlay.over_base(&[], Layer::Domain, "value", "stale"),
            None
        );
    }

    /// [`LayersOverlay::over_base`] resolves an unstaged identity from the
    /// caller-supplied base — the base is the node universe (phase-01 task-4).
    /// Pure in-memory — no path is read.
    #[test]
    fn over_base_resolves_an_unstaged_identity_from_the_supplied_base() {
        let overlay = LayersOverlay::new();
        let kept = node("requirements", "requirement", "untouched");
        assert_eq!(
            overlay.over_base(
                std::slice::from_ref(&kept),
                Layer::Requirements,
                "requirement",
                "untouched"
            ),
            Some(kept)
        );
    }

    /// [`LayersOverlay::over_base`] has **no disk fallback**: an identity
    /// absent from the caller-supplied base resolves as `None` even though a
    /// matching node file exists under the on-disk `apg/layers/**` tree — the
    /// supplied base is the sole universe (phase-01 task-4). Because
    /// `over_base` takes no `apg_root`, an absent-from-base identity can never
    /// synthesize a node from disk; the empty-base assertion is the pure
    /// in-memory proof, and no path is ever read.
    #[test]
    fn over_base_absent_identity_never_falls_back_to_disk() {
        let overlay = LayersOverlay::new();
        assert_eq!(
            overlay.over_base(&[], Layer::Domain, "value", "money"),
            None
        );

        // A base carrying only *other* identities still yields `None` for the
        // absent one (no near-miss per-layer/type/name fallback).
        let other = node("domain", "value", "other");
        assert_eq!(
            overlay.over_base(&[other], Layer::Domain, "value", "money"),
            None
        );
    }

    /// [`ingest_nodes`] is the in-memory core [`ingest_tree`] delegates to
    /// (phase-00 task-2): handed a `Vec<NodeFile>` directly — no disk walk — it
    /// sorts the set, runs the same validation pipeline, and emits one node
    /// record per node plus one edge record per **out**-edge (out is canonical;
    /// the paired in-edge emits nothing). A pairing-inconsistent set (an
    /// out-edge whose other half is absent) is refused, the same
    /// [`check_edge_pairing`] gate `ingest_tree` runs. Pure in-memory — the
    /// scanned/planned universes are empty and no `implemented-by` target is
    /// present, so no code-ref validation fires.
    #[test]
    fn ingest_nodes_emits_records_for_an_in_memory_node_set() {
        // A paired authored edge: `a` depends-on `b` (out in a's file, the
        // matching in-edge in b's).
        let mut a = node("requirements", "requirement", "a");
        a.out
            .push(out_edge("depends-on", "requirements.requirement.b"));
        let mut b = node("requirements", "requirement", "b");
        b.in_edges
            .push(in_edge("depends-on", "requirements.requirement.a"));

        let empty = BTreeSet::new();
        let records = ingest_nodes(&[a, b], &empty, &empty).expect("a paired set ingests");

        // Two node records (sorted: a then b) + exactly one edge record, from
        // a's out-edge; b's in-edge is verified but never emitted.
        assert_eq!(
            records,
            vec![
                Record::Requirement {
                    fqn: "requirements.requirement.a".to_string(),
                    id: String::new(),
                    title: "a".to_string(),
                    body: String::new(),
                    feature: String::new(),
                    properties: NodeProperties::default(),
                },
                Record::DependsOn {
                    from: "requirements.requirement.a".to_string(),
                    to: "requirements.requirement.b".to_string(),
                    properties: NodeProperties::default(),
                },
                Record::Requirement {
                    fqn: "requirements.requirement.b".to_string(),
                    id: String::new(),
                    title: "b".to_string(),
                    body: String::new(),
                    feature: String::new(),
                    properties: NodeProperties::default(),
                },
            ]
        );

        // A pairing-inconsistent set — an out-edge whose matching in-edge is
        // absent from the target's file — is REFUSED, naming the missing half
        // (the same gate `ingest_tree` runs).
        let mut c = node("requirements", "requirement", "c");
        c.out
            .push(out_edge("depends-on", "requirements.requirement.d"));
        let d = node("requirements", "requirement", "d");
        let msg = ingest_nodes(&[c, d], &empty, &empty)
            .unwrap_err()
            .to_string();
        assert!(msg.contains("requirements.requirement.c"), "{msg}");
        assert!(msg.contains("depends-on"), "{msg}");
        assert!(msg.contains("BOTH endpoint files"), "{msg}");
    }

    /// [`properties_json`] / [`properties_from_json`] are exact inverses over
    /// the full properties map: unknown keys and keys carrying an **empty
    /// value** survive the round-trip verbatim, and the canonical encoding is
    /// the sorted-key JSON object (phase-04 task-16). Pure in-memory.
    #[test]
    fn properties_json_round_trips_unknown_and_empty_valued_keys() {
        let mut properties = NodeProperties::new();
        // A known typed key, an unknown metadata key, and an empty-valued key.
        properties.insert("kind".to_string(), "event".to_string());
        properties.insert("unknown-key".to_string(), "kept".to_string());
        properties.insert("empty".to_string(), String::new());

        let encoded = properties_json(&properties);
        // Canonical: sorted keys, every key emitted (including the empty one).
        assert_eq!(
            encoded,
            r#"{"empty":"","kind":"event","unknown-key":"kept"}"#
        );
        assert_eq!(
            properties_from_json(&encoded).expect("canonical JSON decodes"),
            properties
        );
    }

    /// Equal maps encode to byte-identical canonical strings regardless of
    /// insertion order — [`NodeProperties`] is a [`BTreeMap`], whose iteration
    /// order is sorted, so the encoding is deterministic (phase-04 task-16).
    /// Pure in-memory.
    #[test]
    fn properties_json_is_deterministic_for_equal_maps() {
        let mut a = NodeProperties::new();
        a.insert("z".to_string(), "1".to_string());
        a.insert("a".to_string(), "2".to_string());
        a.insert("m".to_string(), String::new());

        // The same map built in a different insertion order.
        let mut b = NodeProperties::new();
        b.insert("m".to_string(), String::new());
        b.insert("a".to_string(), "2".to_string());
        b.insert("z".to_string(), "1".to_string());

        assert_eq!(a, b);
        assert_eq!(properties_json(&a), properties_json(&b));
        assert_eq!(properties_json(&a), r#"{"a":"2","m":"","z":"1"}"#);
    }

    /// The empty map encodes as the canonical `"{}"`, and both `"{}"` and the
    /// empty (or whitespace-only) column value decode back to the empty map —
    /// the projection's representation of an absent/unset property set
    /// (phase-04 task-16). Pure in-memory.
    #[test]
    fn properties_json_and_from_json_agree_on_the_empty_map() {
        assert_eq!(properties_json(&NodeProperties::new()), "{}");
        assert_eq!(
            properties_from_json("{}").expect("empty object decodes"),
            NodeProperties::new()
        );
        assert_eq!(
            properties_from_json("").expect("empty column decodes to empty"),
            NodeProperties::new()
        );
        assert_eq!(
            properties_from_json("   ").expect("blank column decodes to empty"),
            NodeProperties::new()
        );
    }

    /// [`properties_from_json`] refuses malformed input rather than silently
    /// dropping keys: invalid JSON and valid JSON that is **not** a
    /// `string -> string` object (array, number, `null`, string, non-string
    /// value) are each an `Err` (phase-04 task-16). Pure in-memory.
    #[test]
    fn properties_from_json_rejects_malformed_and_non_string_maps() {
        for raw in [
            "not json",
            "{",
            "[1,2]",
            "null",
            "42",
            r#""a string""#,
            r#"{"a":1}"#,
            r#"{"a":null}"#,
            r#"{"a":{"b":"c"}}"#,
        ] {
            assert!(
                properties_from_json(raw).is_err(),
                "`{raw}` must be refused"
            );
        }
    }
}
