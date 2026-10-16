use super::*;

/// unit tier -- pure in-memory: the only unit under test is the shared
/// advisory wording classifier `wording_warning`; no filesystem, database,
/// git or process (`global.constraint.test-tier-boundaries`).
mod unit {
    use super::*;

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
}
