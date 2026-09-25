use super::*;
use crate::testutil::{nf, task_rec};

/// unit tier -- pure in-memory: no filesystem, database, git or process.
/// These operate on in-memory `Record`/`NodeFile` values (validation,
/// rendering, edge linking, coverage semantics).
mod unit {
    use super::*;

    #[test]
    fn link_phase_edges_keeps_all_satisfies_and_preserves_other_phases() {
        // Regression: the removal ran inside the loop (only the last edge
        // survived) and removed edges incident to the phase (incoming Gates
        // from a later phase were severed).
        let mut records = vec![
            Record::PlanPhase {
                fqn: "foo/plan.phase-01".into(),
                number: 1,
                title: "P1".into(),
                deliverable: "D".into(),
                status: "pending".into(),
            },
            Record::PlanPhase {
                fqn: "foo/plan.phase-02".into(),
                number: 2,
                title: "P2".into(),
                deliverable: "D".into(),
                status: "pending".into(),
            },
            // A later phase gating this one: incoming edge, must survive.
            Record::Gates {
                from: "foo/plan.phase-02".into(),
                to: "foo/plan.phase-01".into(),
            },
        ];
        link_phase_edges(
            "foo/plan.phase-01",
            &["R1".into(), "R2".into(), "R3".into()],
            &["3".into()],
            &mut records,
        )
        .unwrap();
        let satisfies: Vec<&str> = records
            .iter()
            .filter_map(|r| match r {
                Record::Satisfies { from, to } if from == "foo/plan.phase-01" => Some(to.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            satisfies,
            vec![
                "requirements.requirement.R1",
                "requirements.requirement.R2",
                "requirements.requirement.R3"
            ]
        );
        // Outgoing Gates set; the incoming phase-02 → phase-01 gate survives.
        assert!(records.iter().any(|r| matches!(
            r,
            Record::Gates { from, to }
                if from == "foo/plan.phase-01" && to == "foo/plan.phase-03"
        )));
        assert!(records.iter().any(|r| matches!(
            r,
            Record::Gates { from, to }
                if from == "foo/plan.phase-02" && to == "foo/plan.phase-01"
        )));
        // Linking phase-01 to gate phase-02 would close the incoming
        // phase-02 → phase-01 gate into a cycle — rejected.
        assert!(link_phase_edges("foo/plan.phase-01", &[], &["2".into()], &mut records,).is_err());
        // Re-linking replaces, never duplicates.
        link_phase_edges("foo/plan.phase-01", &["R9".into()], &[], &mut records).unwrap();
        let satisfies: Vec<&str> = records
            .iter()
            .filter_map(|r| match r {
                Record::Satisfies { from, to } if from == "foo/plan.phase-01" => Some(to.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(satisfies, vec!["requirements.requirement.R9"]);
    }

    #[test]
    fn task_kind_tier_validation() {
        // Default kind is source.
        assert!(validate_task_kind_tier("source", "").is_ok());
        // All four implementer-workable kinds accepted.
        for k in ["source", "test", "gate", "docs"] {
            let tier = if k == "test" { "unit" } else { "" };
            assert!(validate_task_kind_tier(k, tier).is_ok(), "kind {k}");
        }
        // Unknown kinds rejected — the retired `human` kind included.
        assert!(validate_task_kind_tier("qa", "").is_err());
        assert!(validate_task_kind_tier("human", "").is_err());
        // tier required for test, rejected for non-test.
        assert!(validate_task_kind_tier("test", "").is_err());
        assert!(validate_task_kind_tier("source", "unit").is_err());
        // Unknown tier rejected.
        assert!(validate_task_kind_tier("test", "smoke").is_err());
        // All three tiers accepted for test.
        for t in ["unit", "int", "e2e"] {
            assert!(validate_task_kind_tier("test", t).is_ok(), "tier {t}");
        }
    }

    #[test]
    fn render_groups_tasks_by_kind_and_shows_test_tier() {
        let records = vec![
            Record::PlanPhase {
                fqn: "foo/plan.phase-01".into(),
                number: 1,
                title: "P".into(),
                deliverable: "D".into(),
                status: "pending".into(),
            },
            Record::Task {
                fqn: "foo/plan.phase-01.task-1".into(),
                title: "Implement".into(),
                kind: "source".into(),
                tier: String::new(),
                status: "done".into(),
                verb: "creates".into(),
                target: String::new(),
                new_fqn: String::new(),
            },
            Record::Task {
                fqn: "foo/plan.phase-01.task-2".into(),
                title: "Unit tests".into(),
                kind: "test".into(),
                tier: "unit".into(),
                status: "pending".into(),
                verb: "creates".into(),
                target: String::new(),
                new_fqn: String::new(),
            },
            Record::Task {
                fqn: "foo/plan.phase-01.task-3".into(),
                title: "Doc".into(),
                kind: "docs".into(),
                tier: String::new(),
                status: "pending".into(),
                verb: "creates".into(),
                target: String::new(),
                new_fqn: String::new(),
            },
            Record::Contains {
                from: "foo/plan.phase-01".into(),
                to: "foo/plan.phase-01.task-1".into(),
            },
            Record::Contains {
                from: "foo/plan.phase-01".into(),
                to: "foo/plan.phase-01.task-2".into(),
            },
            Record::Contains {
                from: "foo/plan.phase-01".into(),
                to: "foo/plan.phase-01.task-3".into(),
            },
        ];
        let out = render_phase_tasks(&records, "foo/plan.phase-01");
        // Grouped by kind in canonical order (source before test before docs).
        let source_pos = out.find("**source**").unwrap();
        let test_pos = out.find("**test**").unwrap();
        let docs_pos = out.find("**docs**").unwrap();
        assert!(source_pos < test_pos && test_pos < docs_pos, "order: {out}");
        // Test depth renders as test/unit.
        assert!(
            out.contains("- [ ] `foo/plan.phase-01.task-2` — Unit tests (test/unit)"),
            "{out}"
        );
        // Done checkbox preserved.
        assert!(
            out.contains("- [x] `foo/plan.phase-01.task-1` — Implement (source)"),
            "{out}"
        );
    }

    #[test]
    fn plan_add_phase_rejects_transitive_gate_cycle() {
        // Phase-1 gates on phase-2, phase-2 gates on phase-3. Adding
        // phase-1 → phase-3 closes 1 → 3 → 2 → 1 — the same
        // `cycle_closing_path` machinery `apg plan link` uses now guards the
        // `plan add phase --prereq` path too (REVIEW: the add arm used to push
        // Gates with no cycle check). Mirrors the spec-side phase_gate_edge
        // test.
        let records = vec![
            Record::PlanPhase {
                fqn: "foo/plan.phase-01".into(),
                number: 1,
                title: "P1".into(),
                deliverable: String::new(),
                status: String::new(),
            },
            Record::PlanPhase {
                fqn: "foo/plan.phase-02".into(),
                number: 2,
                title: "P2".into(),
                deliverable: String::new(),
                status: String::new(),
            },
            Record::PlanPhase {
                fqn: "foo/plan.phase-03".into(),
                number: 3,
                title: "P3".into(),
                deliverable: String::new(),
                status: String::new(),
            },
            Record::Gates {
                from: "foo/plan.phase-02".into(),
                to: "foo/plan.phase-01".into(),
            },
            Record::Gates {
                from: "foo/plan.phase-03".into(),
                to: "foo/plan.phase-02".into(),
            },
        ];
        let err = push_gate("foo/plan.phase-01", "foo/plan.phase-03", &records).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("would create a cycle"), "got: {msg}");
        assert!(msg.contains("01 → 03"), "got: {msg}");
        // A self-gate is rejected (dedicated cycle path).
        let err = push_gate("foo/plan.phase-01", "foo/plan.phase-01", &records).unwrap_err();
        assert!(
            format!("{err:#}").contains("would create a cycle"),
            "got: {err:#}"
        );
        // A benign gate (phase-01 → phase-04) closes nothing.
        push_gate("foo/plan.phase-01", "foo/plan.phase-04", &records).unwrap();
    }

    #[test]
    fn coverage_check_reports_gaps_no_claims_and_verb_agnostic_touches() {
        // Pure semantics: exact-FQN touch matching, verb-agnostic (a deletes
        // task touches what it deletes), a renames/moves new_fqn covering the
        // destination, a creates covering a planned FQN, empty-target tasks
        // touching nothing, and non-solution nodes (person, solution notes)
        // ignored. The solution nodes are branch-added; the delta is supplied
        // directly here (the git compare is exercised by the int test).
        let records = vec![
            Record::Plan {
                fqn: "foo/plan".to_string(),
                title: "P".to_string(),
                strategy: String::new(),
            },
            Record::Task {
                fqn: "foo/plan.phase-01.task-1".to_string(),
                title: "T1".to_string(),
                kind: "source".to_string(),
                tier: String::new(),
                status: "pending".to_string(),
                verb: "modifies".to_string(),
                target: "github.com/x/y.Store".to_string(),
                new_fqn: String::new(),
            },
            Record::Task {
                fqn: "foo/plan.phase-01.task-2".to_string(),
                title: "T2".to_string(),
                kind: "source".to_string(),
                tier: String::new(),
                status: "pending".to_string(),
                verb: "deletes".to_string(),
                target: "github.com/x/y.Store".to_string(),
                new_fqn: String::new(),
            },
            Record::Task {
                fqn: "foo/plan.phase-01.task-3".to_string(),
                title: "T3".to_string(),
                kind: "source".to_string(),
                tier: String::new(),
                status: "pending".to_string(),
                verb: "renames".to_string(),
                target: "github.com/x/y.Old".to_string(),
                new_fqn: "github.com/x/y.Store2".to_string(),
            },
            Record::Task {
                fqn: "foo/plan.phase-01.task-4".to_string(),
                title: "T4".to_string(),
                kind: "source".to_string(),
                tier: String::new(),
                status: "pending".to_string(),
                verb: "creates".to_string(),
                target: "github.com/x/y.Gateway".to_string(),
                new_fqn: String::new(),
            },
            // A target-less creates touches nothing.
            Record::Task {
                fqn: "foo/plan.phase-01.task-5".to_string(),
                title: "T5".to_string(),
                kind: "source".to_string(),
                tier: String::new(),
                status: "pending".to_string(),
                verb: "creates".to_string(),
                target: String::new(),
                new_fqn: String::new(),
            },
            // The planned-node declaration is irrelevant to coverage — the
            // creates task's touch is what counts (edge case: an
            // implemented-by FQN already `status: planned` in the DB,
            // awaiting its creates task).
            Record::PlannedNode {
                fqn: "github.com/x/y.Gateway".to_string(),
                kind: "struct".to_string(),
                name: "Gateway".to_string(),
                parent: String::new(),
            },
        ];
        let node = |t: &str, n: &str, refs: &[&str]| crate::layers::NodeFile {
            layer: "solution".to_string(),
            node_type: t.to_string(),
            name: n.to_string(),
            body: String::new(),
            properties: std::collections::BTreeMap::new(),
            out: refs
                .iter()
                .map(|r| crate::layers::OutEdge {
                    kind: "implemented-by".to_string(),
                    target: r.to_string(),
                    properties: std::collections::BTreeMap::new(),
                })
                .collect(),
            in_edges: Vec::new(),
        };
        let nodes = vec![
            node(
                "system",
                "payments",
                &["github.com/x/y.Store", "github.com/x/y.Gateway"],
            ),
            node("container", "api", &["github.com/x/y.Store2"]),
            // An untouched implemented-by FQN -> the gap.
            node("component", "reporting", &["github.com/x/y.Reporting"]),
            // No implemented-by edge -> exempt, reported as a warning.
            node("component", "checkout", &[]),
            // Not a solution kind -> ignored entirely.
            node("person", "ops", &["github.com/x/y.Store"]),
            crate::layers::NodeFile {
                layer: "solution".to_string(),
                node_type: "note".to_string(),
                name: "design-note".to_string(),
                body: String::new(),
                properties: std::collections::BTreeMap::new(),
                out: vec![crate::layers::OutEdge {
                    kind: "implemented-by".to_string(),
                    target: "github.com/x/y.Store".to_string(),
                    properties: std::collections::BTreeMap::new(),
                }],
                in_edges: Vec::new(),
            },
        ];
        // The nodes are branch-added (the pure test supplies the delta
        // directly — the git half is exercised by the int test).
        let branch_added: BTreeSet<String> = [
            "solution.system.payments",
            "solution.container.api",
            "solution.component.reporting",
            "solution.component.checkout",
        ]
        .into_iter()
        .map(str::to_string)
        .collect();
        let report = coverage_check(&records, &nodes, &BTreeSet::new(), &branch_added);
        // Only the reporting component's FQN is untouched: Store is touched
        // (modifies AND deletes — verb-agnostic), Gateway by the creates,
        // Store2 by the rename's new_fqn.
        assert_eq!(
            report.gaps,
            vec![CoverageGap {
                solution: "solution.component.reporting".to_string(),
                fqn: "github.com/x/y.Reporting".to_string(),
            }]
        );
        assert_eq!(report.no_claims, vec!["solution.component.checkout"]);
    }

    /// The coverage gate is language-root agnostic: task targets / rename
    /// destinations may be language-rooted while the solution nodes'
    /// `implemented-by` targets are bare (and the reverse) — the identities
    /// match, so there are NO gaps either way.
    #[test]
    fn coverage_identity_tolerates_language_root_on_both_sides() {
        let task = |fqn: &str, verb: &str, target: &str, new_fqn: &str| Record::Task {
            fqn: fqn.to_string(),
            title: "T".to_string(),
            kind: "source".to_string(),
            tier: String::new(),
            status: "pending".to_string(),
            verb: verb.to_string(),
            target: target.to_string(),
            new_fqn: new_fqn.to_string(),
        };
        let node = |name: &str, refs: &[&str]| crate::layers::NodeFile {
            layer: "solution".to_string(),
            node_type: "system".to_string(),
            name: name.to_string(),
            body: String::new(),
            properties: std::collections::BTreeMap::new(),
            out: refs
                .iter()
                .map(|r| crate::layers::OutEdge {
                    kind: "implemented-by".to_string(),
                    target: r.to_string(),
                    properties: std::collections::BTreeMap::new(),
                })
                .collect(),
            in_edges: Vec::new(),
        };
        let branch_added: BTreeSet<String> = ["solution.system.alpha", "solution.system.beta"]
            .into_iter()
            .map(str::to_string)
            .collect();

        // Rooted task targets, BARE implemented-by targets.
        let rooted_tasks = vec![
            task("foo/plan.phase-01.task-1", "modifies", "rust.apg.cache", ""),
            task(
                "foo/plan.phase-01.task-2",
                "renames",
                "rust.apg.old_mod",
                "rust.apg.new_mod",
            ),
        ];
        let bare_refs = vec![
            node("alpha", &["apg.cache"]),
            node("beta", &["apg.new_mod"]),
        ];
        let report = coverage_check(&rooted_tasks, &bare_refs, &BTreeSet::new(), &branch_added);
        assert!(
            report.gaps.is_empty(),
            "rooted task targets must cover bare implemented-by targets: {:?}",
            report.gaps
        );

        // BARE task targets, ROOTED implemented-by targets (the reverse).
        let bare_tasks = vec![
            task("foo/plan.phase-01.task-1", "modifies", "apg.cache", ""),
            task(
                "foo/plan.phase-01.task-2",
                "renames",
                "apg.old_mod",
                "apg.new_mod",
            ),
        ];
        let rooted_refs = vec![
            node("alpha", &["rust.apg.cache"]),
            node("beta", &["rust.apg.new_mod"]),
        ];
        let report = coverage_check(&bare_tasks, &rooted_refs, &BTreeSet::new(), &branch_added);
        assert!(
            report.gaps.is_empty(),
            "bare task targets must cover rooted implemented-by targets: {:?}",
            report.gaps
        );
    }

    /// `realization_candidates` (pure) offers the authored FQN, its identity,
    /// and every `<root>.<identity>` variant — the set whose real realization
    /// satisfies the planned-node gate under either rooting.
    #[test]
    fn realization_candidates_cover_both_rootings() {
        let rooted = realization_candidates("rust.apg.cache");
        assert!(rooted.contains(&"rust.apg.cache".to_string()));
        assert!(rooted.contains(&"apg.cache".to_string()));
        assert!(rooted.contains(&"java.apg.cache".to_string()));
        assert!(!rooted.contains(&"rust.rust.apg.cache".to_string()));

        let bare = realization_candidates("apg.cache");
        assert!(bare.contains(&"apg.cache".to_string()));
        assert!(bare.contains(&"rust.apg.cache".to_string()));
        assert!(bare.contains(&"go.apg.cache".to_string()));
    }

    // ------------------------------------------------------------------
    // phase-06 (spine-scoped coverage): the pure coverage_check semantics —
    // spine reachability, the branch delta, the pre-existing exemption, and
    // the no-claims exemption — plus the plan_verify_at int path that supplies
    // the real satisfied set and default-branch delta.
    // ------------------------------------------------------------------

    #[test]
    fn coverage_spine_reachability_scopes_required_solution_nodes() {
        // Only solution nodes reached from a SATISFIED requirement through
        // Requirement Drives Domain RealisedBy Solution are required; an
        // unrelated (non-branch-added) solution node is ignored — even an
        // untouched implemented-by FQN on it yields no gap.
        let nodes = vec![
            nf(
                "requirements",
                "requirement",
                "cr",
                &[("drives", "domain.entity.plan-record")],
            ),
            nf(
                "domain",
                "entity",
                "plan-record",
                &[("realised-by", "solution.component.reached")],
            ),
            nf(
                "solution",
                "component",
                "reached",
                &[("implemented-by", "github.com/x/y.Store")],
            ),
            nf(
                "solution",
                "component",
                "unrelated",
                &[("implemented-by", "github.com/x/y.Untouched")],
            ),
        ];
        let satisfied: BTreeSet<String> = ["requirements.requirement.cr".to_string()]
            .into_iter()
            .collect();

        // The reached node's FQN is touched -> no gap; the unrelated node's
        // untouched FQN is never considered.
        let records = vec![task_rec("modifies", "github.com/x/y.Store", "")];
        let report = coverage_check(&records, &nodes, &satisfied, &BTreeSet::new());
        assert!(report.gaps.is_empty(), "{report:?}");

        // Leave the reached node's FQN untouched -> exactly that gap (the
        // unrelated node stays out of scope).
        let records = vec![task_rec("creates", "github.com/x/y.Other", "")];
        let report = coverage_check(&records, &nodes, &satisfied, &BTreeSet::new());
        assert_eq!(
            report.gaps,
            vec![CoverageGap {
                solution: "solution.component.reached".to_string(),
                fqn: "github.com/x/y.Store".to_string(),
            }]
        );
    }

    #[test]
    fn coverage_branch_added_solution_nodes_are_required_without_a_spine() {
        // A branch-added solution node's implemented-by FQN is required even
        // when no satisfied requirement reaches it; a branch-added node with
        // NO implemented-by edge stays exempt via CoverageReport::no_claims
        // (the branch-added plan-store-atomic-rewrite forces no fake task).
        let nodes = vec![
            nf(
                "solution",
                "component",
                "added",
                &[("implemented-by", "github.com/x/y.Store")],
            ),
            nf("solution", "component", "plan-store-atomic-rewrite", &[]),
        ];
        let branch_added: BTreeSet<String> = [
            "solution.component.added".to_string(),
            "solution.component.plan-store-atomic-rewrite".to_string(),
        ]
        .into_iter()
        .collect();

        let records = vec![task_rec("modifies", "github.com/x/y.Store", "")];
        let report = coverage_check(&records, &nodes, &BTreeSet::new(), &branch_added);
        assert!(report.gaps.is_empty(), "{report:?}");
        assert_eq!(
            report.no_claims,
            vec!["solution.component.plan-store-atomic-rewrite"]
        );

        // The branch-added node's FQN untouched -> a gap even with no spine.
        let records = vec![task_rec("creates", "github.com/x/y.Other", "")];
        let report = coverage_check(&records, &nodes, &BTreeSet::new(), &branch_added);
        assert_eq!(
            report.gaps,
            vec![CoverageGap {
                solution: "solution.component.added".to_string(),
                fqn: "github.com/x/y.Store".to_string(),
            }]
        );
    }

    #[test]
    fn coverage_exempts_unreachable_pre_existing_solution_nodes() {
        // The merged worktree-cleanup nodes are pre-existing (present on the
        // default branch) and unreachable from any satisfied requirement, so
        // they yield no gaps and force no fake modifies tasks.
        let nodes = vec![
            nf(
                "solution",
                "component",
                "project-delete",
                &[("implemented-by", "apg.project_cmd.delete_project")],
            ),
            nf(
                "solution",
                "component",
                "project-dispatch",
                &[("implemented-by", "apg.project_cmd.cmd_project")],
            ),
            nf(
                "solution",
                "component",
                "project-merge",
                &[("implemented-by", "apg.project_cmd.project_merge_at")],
            ),
            nf(
                "solution",
                "container",
                "project-command",
                &[("implemented-by", "apg.project_cmd.cmd_project")],
            ),
        ];
        let records = vec![task_rec("creates", "github.com/x/y.Gateway", "")];
        let report = coverage_check(&records, &nodes, &BTreeSet::new(), &BTreeSet::new());
        assert!(report.gaps.is_empty(), "{report:?}");
        assert!(report.no_claims.is_empty(), "{report:?}");
    }
}
