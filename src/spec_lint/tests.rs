use super::*;

/// unit tier -- pure in-memory: the only unit under test is the shared
/// advisory wording classifier `wording_warning`; no filesystem, database,
/// git or process (`global.constraint.test-tier-boundaries`).
mod unit {
    use super::*;
    use crate::layers::{Context, Layer};
    use crate::testutil::{out_edge, valid_model};

    /// Drop one out-edge from the named fixture node (the matching in half is
    /// irrelevant here: these tests exercise the pure cardinality/matrix
    /// validators, never the file-pairing invariant).
    fn without_edge(
        nodes: &mut [NodeFile],
        layer: &str,
        node_type: &str,
        name: &str,
        kind: &str,
        target: &str,
    ) {
        nodes
            .iter_mut()
            .find(|n| n.layer == layer && n.node_type == node_type && n.name == name)
            .unwrap_or_else(|| panic!("fixture node {layer}.{node_type}.{name} is missing"))
            .out
            .retain(|e| !(e.kind == kind && e.target == target));
    }

    /// `wording_warning` flags the three word classes a reviewer is likely to
    /// reject in a tier-1-3 node body — negation (`not`/`never`/`no`),
    /// future-tense (`will`/`shall`) and time-relative
    /// (`today`/`now`/`currently`/`no longer`/`was`) — returning the shared
    /// [`WORDING_ADVISORY`], and is silent on plain present-tense prose.
    #[test]
    fn wording_warning_flags_negation_future_and_time_words() {
        // Negation.
        for body in [
            "The system does not store passwords.",
            "Never delete a record.",
            "No customer sees another customer's data.",
        ] {
            assert_eq!(
                wording_warning(body),
                Some(WORDING_ADVISORY),
                "negation must be flagged: {body}"
            );
        }

        // Future-tense / obligation.
        for body in [
            "The service will retry the request.",
            "The operator shall approve every change.",
        ] {
            assert_eq!(
                wording_warning(body),
                Some(WORDING_ADVISORY),
                "future-tense wording must be flagged: {body}"
            );
        }

        // Time-relative.
        for body in [
            "Today the price is fixed.",
            "The job now runs hourly.",
            "Currently the queue drains nightly.",
            "The account is no longer active.",
            "The old flow was synchronous.",
        ] {
            assert_eq!(
                wording_warning(body),
                Some(WORDING_ADVISORY),
                "time-relative wording must be flagged: {body}"
            );
        }

        // Plain present-tense prose is silent.
        for body in [
            "A customer places an order.",
            "The service stores the record.",
            "Each order carries a total.",
            "The system reuses the cached value.",
            "",
        ] {
            assert_eq!(
                wording_warning(body),
                None,
                "plain present-tense prose must stay silent: {body}"
            );
        }
    }

    /// The note-confinement rule (`global.constraint.note-implementation-only`)
    /// is the pure helper [`note_confinement_errors`]: a `note` in the
    /// requirements, domain, solution or global layer yields exactly one error
    /// naming the rule and the offending layer, an `implementation`-layer note
    /// yields none, and a non-`note` node in any layer yields none.
    ///
    /// The helper is pure over in-memory `NodeFile`s — the only unit under test
    /// (assembly of the error strings) — so the test is `unit`, never `e2e`,
    /// even though the production caller [`lint`] is I/O-bound.
    #[test]
    fn note_confinement_errors_confines_notes_to_implementation() {
        fn note_in(layer: &str) -> NodeFile {
            NodeFile {
                layer: layer.to_string(),
                node_type: "note".to_string(),
                name: format!("{layer}-note"),
                body: String::new(),
                properties: Default::default(),
                out: Vec::new(),
                in_edges: Vec::new(),
            }
        }

        // A note in any non-implementation layer is one error naming the rule.
        for layer in ["requirements", "domain", "solution", "global"] {
            let errors = note_confinement_errors(&[note_in(layer)]);
            assert_eq!(errors.len(), 1, "a `{layer}` note must be one error");
            assert!(
                errors[0].contains("global.constraint.note-implementation-only"),
                "the error must name the rule: {}",
                errors[0]
            );
            assert!(
                errors[0].contains(&format!("layer `{layer}`")),
                "the error must name the offending layer: {}",
                errors[0]
            );
        }

        // The implementation layer is the note's sanctioned home.
        assert!(
            note_confinement_errors(&[note_in("implementation")]).is_empty(),
            "an implementation-layer note must pass"
        );

        // A non-note node in any layer never trips the note-confinement rule.
        let requirement = NodeFile {
            layer: "requirements".to_string(),
            node_type: "requirement".to_string(),
            name: "place-order".to_string(),
            body: String::new(),
            properties: Default::default(),
            out: Vec::new(),
            in_edges: Vec::new(),
        };
        assert!(
            note_confinement_errors(&[requirement]).is_empty(),
            "a non-note node must never trip the note-confinement rule"
        );
    }

    /// The details-shape rule (`global.constraint.note-details-shape`,
    /// `domain.constraint.details-canonical-target-set`) is the pure helper
    /// [`details_shape_errors`]: a note whose `details` edge names exactly one
    /// **code** FQN yields no error, while a note with zero or two-plus details
    /// edges, or whose single details target is an authored-node FQN (any
    /// layer, including another note), yields exactly one error naming the
    /// code-FQN requirement.
    ///
    /// The helper is pure over in-memory `NodeFile`s — the only unit under test
    /// (assembly of the error strings) — so the test is `unit`, never `e2e`,
    /// even though the production caller [`lint`] is I/O-bound.
    #[test]
    fn details_shape_errors_requires_exactly_one_code_fqn_target() {
        use crate::testutil::{node, out_edge};

        fn note(name: &str, targets: &[&str]) -> NodeFile {
            let mut n = node("implementation", "note", name);
            n.out = targets.iter().map(|t| out_edge("details", t)).collect();
            n
        }

        // One code FQN target is the sanctioned shape — no error.
        assert!(
            details_shape_errors(&[note("deepens-lint", &["rust.apg.spec_lint.lint"])]).is_empty(),
            "a note with exactly one code FQN details target must pass"
        );

        // Zero details edges is off-model — one error naming the code-FQN rule.
        let zero = details_shape_errors(&[note("no-target", &[])]);
        assert_eq!(
            zero.len(),
            1,
            "a note with no details edge must be one error"
        );
        assert!(
            zero[0].contains("code FQN"),
            "the error must name the code-FQN requirement: {}",
            zero[0]
        );

        // Two details edges is off-model — one error naming the code-FQN rule.
        let two = details_shape_errors(&[note(
            "two-targets",
            &[
                "rust.apg.spec_lint.lint",
                "rust.apg.spec_lint.wording_warning",
            ],
        )]);
        assert_eq!(
            two.len(),
            1,
            "a note with two details edges must be one error"
        );
        assert!(
            two[0].contains("code FQN"),
            "the error must name the code-FQN requirement: {}",
            two[0]
        );

        // A single authored-node target (any layer, including another note) is
        // off-model — one error naming the code-FQN rule.
        for authored in ["requirements.requirement.r1", "requirements.note.n2"] {
            let errors = details_shape_errors(&[note("authored-target", &[authored])]);
            assert_eq!(
                errors.len(),
                1,
                "an authored details target `{authored}` must be one error"
            );
            assert!(
                errors[0].contains("code FQN"),
                "the error must name the code-FQN requirement: {}",
                errors[0]
            );
        }
    }

    /// The tier-1-3 body-budget rule
    /// (`requirements.requirement.spec-body-budget`) is the pure helper
    /// [`body_budget_errors`]: a node in a tier-1-3 layer
    /// (`requirements`/`domain`/`solution`) whose body is **at most**
    /// [`TIER_BODY_BUDGET`] (244) **characters** passes, a body one character
    /// over budget yields exactly one error naming the node FQN, its character
    /// count and the budget, and a non-tier node (`global`/`implementation`) is
    /// never subject to the budget however long its body.
    ///
    /// The helper is pure over in-memory `NodeFile`s — the only unit under test
    /// (assembly of the error strings) — so the test is `unit`, never `e2e`,
    /// even though the production caller [`lint`] is I/O-bound.
    #[test]
    fn body_budget_errors_caps_tier_1_3_bodies_at_244_chars() {
        use crate::testutil::node;

        fn with_body(layer: &str, node_type: &str, name: &str, body: String) -> NodeFile {
            let mut n = node(layer, node_type, name);
            n.body = body;
            n
        }

        // One representative node per tier-1-3 layer.
        let tier_1_3 = [
            ("requirements", "requirement"),
            ("domain", "entity"),
            ("solution", "container"),
        ];

        // A body of exactly the budget is "at most 244" — no error.
        for (layer, node_type) in tier_1_3 {
            let at_budget = with_body(layer, node_type, "at-budget", "a".repeat(TIER_BODY_BUDGET));
            assert!(
                body_budget_errors(&[at_budget]).is_empty(),
                "a `{layer}` body of exactly {TIER_BODY_BUDGET} chars is at budget and must pass"
            );
        }

        // One character over budget — exactly one error naming the node, its
        // character count and the budget.
        for (layer, node_type) in tier_1_3 {
            let over = with_body(
                layer,
                node_type,
                "over-budget",
                "a".repeat(TIER_BODY_BUDGET + 1),
            );
            let errors = body_budget_errors(&[over]);
            assert_eq!(
                errors.len(),
                1,
                "a `{layer}` body of {} chars is over budget and must be one error",
                TIER_BODY_BUDGET + 1
            );
            assert!(
                errors[0].contains(&format!("{layer}.{node_type}.over-budget")),
                "the error must name the offending node FQN: {}",
                errors[0]
            );
            assert!(
                errors[0].contains(&(TIER_BODY_BUDGET + 1).to_string()),
                "the error must name the body's character count: {}",
                errors[0]
            );
            assert!(
                errors[0].contains(&TIER_BODY_BUDGET.to_string()),
                "the error must name the budget: {}",
                errors[0]
            );
        }

        // The rule is tier-1-3 only: a non-tier node with a far-over-budget
        // body is never subject to it.
        for (layer, node_type) in [("global", "constraint"), ("implementation", "note")] {
            let over = with_body(
                layer,
                node_type,
                "off-tier",
                "a".repeat(TIER_BODY_BUDGET + 100),
            );
            assert!(
                body_budget_errors(&[over]).is_empty(),
                "a non-tier `{layer}` node is not subject to the body budget"
            );
        }
    }

    /// `--tier` parsing (SPEC §9.4): the approved scopes resolve to their
    /// layers, omission is all tiers, and every usage error the spec names —
    /// unknown flag, repeated `--tier`, missing/invalid value, and unexpected
    /// positional argument — is refused. `implementation` is outside the
    /// tier-1-3 document, so it is a usage error rather than a silent no-op.
    #[test]
    fn tier_flag_parses_the_approved_scope_and_refuses_usage_errors() {
        let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();

        assert_eq!(parse_lint_args(&args(&[])).unwrap(), None);
        for (value, layer) in [
            ("requirements", Layer::Requirements),
            ("domain", Layer::Domain),
            ("solution", Layer::Solution),
        ] {
            assert_eq!(
                parse_lint_args(&args(&["--tier", value])).unwrap(),
                Some(layer)
            );
        }

        for bad in [
            &["--tier"][..],
            &["--tier", "implementation"],
            &["--tier", "bogus"],
            &["--tier", "domain", "--tier", "solution"],
            &["--bogus"],
            &["extra"],
            &["--tier", "domain", "extra"],
        ] {
            let err = parse_lint_args(&args(bad)).unwrap_err().to_string();
            assert!(
                err.contains("usage:"),
                "`{bad:?}` must be a usage error, got: {err}"
            );
        }
    }

    /// Scope is separate from context (SPEC §9.2): a domain cardinality
    /// violation is reported by all-tier and domain lint, and is invisible to
    /// requirements lint; a requirements violation is the mirror image. The
    /// complete fixture passes every scope.
    #[test]
    fn tier_scope_hides_other_tiers_violations() {
        assert!(model_errors(&valid_model(), None, Context::Lint).is_empty());
        for scope in [Layer::Requirements, Layer::Domain, Layer::Solution] {
            assert!(
                model_errors(&valid_model(), Some(scope), Context::Lint).is_empty(),
                "the complete model must pass the `{}` scope",
                scope.layer_dir()
            );
        }

        // Domain: a Service realised by no Component.
        let mut domain_broken = valid_model();
        without_edge(
            &mut domain_broken,
            "solution",
            "component",
            "order-handler",
            "realises",
            "domain.service.place-order",
        );
        let domain_rule = "Service `domain.service.place-order` is realised by no Component";
        assert!(
            model_errors(&domain_broken, None, Context::Lint)
                .iter()
                .any(|e| e.contains(domain_rule)),
            "all-tier lint must report the domain violation"
        );
        assert!(
            model_errors(&domain_broken, Some(Layer::Domain), Context::Lint)
                .iter()
                .any(|e| e.contains(domain_rule)),
            "domain lint must report the domain violation"
        );
        assert!(
            model_errors(&domain_broken, Some(Layer::Requirements), Context::Lint).is_empty(),
            "requirements lint must not report a domain cardinality violation"
        );

        // Requirements: a User requiring no Requirement.
        let mut requirements_broken = valid_model();
        without_edge(
            &mut requirements_broken,
            "requirements",
            "user",
            "customer",
            "requires",
            "requirements.requirement.place-order",
        );
        let requirements_rule = "`requirements.user.customer` requires no Requirement";
        assert!(
            model_errors(&requirements_broken, None, Context::Lint)
                .iter()
                .any(|e| e.contains(requirements_rule)),
            "all-tier lint must report the requirements violation"
        );
        assert!(
            model_errors(
                &requirements_broken,
                Some(Layer::Requirements),
                Context::Lint
            )
            .iter()
            .any(|e| e.contains(requirements_rule)),
            "requirements lint must report the requirements violation"
        );
        assert!(
            model_errors(&requirements_broken, Some(Layer::Domain), Context::Lint).is_empty(),
            "domain lint must not report a requirements cardinality violation"
        );
    }

    /// The shared mechanical validator set surfaces through lint (SPEC §9.1):
    /// [`model_errors`] reports the SPEC §4 matrix's refusal for an invalid
    /// edge shape — with the identical message the write path's
    /// [`validate_edges`] produces — and a dangling authored endpoint.
    #[test]
    fn shared_matrix_and_endpoint_existence_errors_surface() {
        let mut bad_shape = valid_model();
        bad_shape
            .iter_mut()
            .find(|n| n.layer == "domain" && n.node_type == "service" && n.name == "place-order")
            .unwrap()
            .out
            .push(out_edge("driven-by", "domain.group.ordering"));

        let direct = validate_edges(&[(
            "driven-by",
            "domain.service.place-order",
            "domain.group.ordering",
        )])
        .unwrap_err()
        .to_string();
        assert!(
            model_errors(&bad_shape, None, Context::Lint).contains(&direct),
            "lint must report the write path's matrix message verbatim: {direct}"
        );

        let mut dangling = valid_model();
        dangling
            .iter_mut()
            .find(|n| n.layer == "domain" && n.node_type == "service" && n.name == "place-order")
            .unwrap()
            .out
            .push(out_edge("driven-by", "requirements.requirement.missing"));
        assert!(
            model_errors(&dangling, None, Context::Lint)
                .iter()
                .any(|e| e
                    .contains("edge `driven-by` endpoint `requirements.requirement.missing`")
                    && e.contains("dangling authored reference")),
            "lint must report a dangling authored endpoint"
        );
    }

    /// The event-only `publishes`/`subscribes` target rule (SPEC §9.1) reaches
    /// lint through the SAME shared validator the write path runs: a Service
    /// that `publishes` a plain `Entity (kind: entity)` is reported by
    /// [`model_errors`] with the identical message
    /// [`validate_assembled_rules`] produces.
    #[test]
    fn event_only_publish_target_is_reported_with_the_write_path_message() {
        use crate::layers::validate_assembled_rules;

        let mut model = valid_model();
        model
            .iter_mut()
            .find(|n| n.layer == "domain" && n.node_type == "service" && n.name == "place-order")
            .unwrap()
            .out
            .push(out_edge("publishes", "domain.entity.order"));

        let assembled: BTreeMap<String, &NodeFile> = model
            .iter()
            .map(|n| (fqn(layer_of(&n.layer), &n.node_type, &n.name), n))
            .collect();
        let universe: BTreeSet<(Layer, String, String)> =
            assembled.keys().filter_map(|f| parse_fqn(f).ok()).collect();
        let write_path = validate_assembled_rules(&assembled, &universe)
            .unwrap_err()
            .to_string();

        let errors = model_errors(&model, None, Context::Lint);
        assert!(
            write_path.contains("must be an Entity with kind `event`"),
            "the write path must refuse a non-event publish target: {write_path}"
        );
        assert!(
            errors.contains(&write_path),
            "lint must report the write path's event-target message verbatim: {write_path}\n{errors:?}"
        );
    }

    /// Scoped lint flags a dangling authored endpoint on an in-scope edge (the
    /// SPEC §9.2 "drift" reading): endpoint existence is structural, not a
    /// "downstream completeness" question, so a `--tier requirements` run
    /// reports a `driven-by` edge whose target Requirement is missing (the
    /// source Service is the more-concrete upstream tier).
    #[test]
    fn requirements_scope_flags_a_dangling_driven_by_target() {
        let mut dangling = valid_model();
        dangling
            .iter_mut()
            .find(|n| n.layer == "domain" && n.node_type == "service" && n.name == "place-order")
            .unwrap()
            .out
            .push(out_edge("driven-by", "requirements.requirement.missing"));

        let errors = model_errors(&dangling, Some(Layer::Requirements), Context::Lint);
        assert!(
            errors.iter().any(|e| e
                .contains("edge `driven-by` endpoint `requirements.requirement.missing`")
                && e.contains("dangling authored reference")),
            "requirements scope must flag the dangling authored endpoint as drift: {errors:?}"
        );
    }

    /// The sole deferred obligation (SPEC §8.2/§9.2): a Component's missing
    /// `implemented-by` claim is permitted in the `Save` and `Lint` contexts
    /// and is an error under `MergeStrength`; one present claim satisfies
    /// merge strength, and more than one is an error in every context.
    #[test]
    fn implemented_by_presence_depends_on_context_not_scope() {
        let model = valid_model();
        for context in [Context::Save, Context::Lint] {
            assert!(
                model_errors(&model, None, context).is_empty(),
                "a missing Component claim is deferred under {context:?}"
            );
        }
        assert!(
            model_errors(&model, Some(Layer::Solution), Context::MergeStrength)
                .iter()
                .any(|e| e.contains("has no `implemented-by` claim")),
            "merge strength must refuse a missing Component code link"
        );

        let mut linked = valid_model();
        linked
            .iter_mut()
            .find(|n| n.layer == "solution" && n.name == "order-handler")
            .unwrap()
            .out
            .push(out_edge("implemented-by", "rust.apg.main"));
        assert!(
            model_errors(&linked, None, Context::MergeStrength).is_empty(),
            "one present claim satisfies merge strength"
        );

        linked
            .iter_mut()
            .find(|n| n.layer == "solution" && n.name == "order-handler")
            .unwrap()
            .out
            .push(out_edge("implemented-by", "rust.apg.spec_lint.lint"));
        assert!(
            model_errors(&linked, None, Context::Lint).is_empty(),
            "a component may claim more than one code node (no at-most-one restriction)"
        );
    }
}
