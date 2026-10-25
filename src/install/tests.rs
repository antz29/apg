use super::*;

/// unit tier -- pure in-memory: no filesystem, database, git or process. The
/// changelog slicer is one pure function (`render_migration_guide`) over a
/// changelog string; the version comparisons and the section extraction are
/// its private parts, tested through it.
mod unit {
    use super::*;

    /// A synthetic changelog exercising every convention: a `## vX.Y.Z` header
    /// per entry, an inline `### Migration` mention in the intro prose (never a
    /// heading, so ignored), entries with and without a Migration section, and
    /// `0.22.10` after `0.22.2` so numeric — not lexical — patch ordering shows.
    const LOG: &str = "\
# Changelog

Prose mentioning `### Migration` inline, which is not a heading.

## v0.25.0

Notes for 0.25.

### Migration

- do the 0.25 thing
- do the second 0.25 thing

## v0.24.0

No model change.

## v0.23.0

### Migration

- reverse the spine
- invent the role

## v0.22.10

### Migration

- patch-ordered task

## v0.22.2

Patch only.
";

    #[test]
    fn fresh_init_selects_nothing() {
        let guide = render_migration_guide(LOG, None, "0.25.0");
        assert!(
            guide.contains("No migration required"),
            "a fresh init has no prior version: {guide}"
        );
        assert!(!guide.contains("do the 0.25 thing"), "{guide}");
    }

    #[test]
    fn range_is_old_exclusive_and_new_inclusive() {
        let guide = render_migration_guide(LOG, Some("0.23.0"), "0.25.0");
        assert!(guide.contains("0.23.0 → 0.25.0"), "{guide}");
        assert!(guide.contains("do the 0.25 thing"), "{guide}");
        // v0.23.0 is the OLD bound: strictly greater excludes it.
        assert!(
            !guide.contains("reverse the spine"),
            "the old version's own section is excluded: {guide}"
        );
        assert!(!guide.contains("patch-ordered task"), "{guide}");
    }

    #[test]
    fn concatenates_selected_sections_in_ascending_version_order() {
        let guide = render_migration_guide(LOG, Some("0.22.2"), "0.25.0");
        let patch = guide
            .find("patch-ordered task")
            .expect("0.22.10 is in range");
        let spine = guide.find("reverse the spine").expect("0.23.0 is in range");
        let last = guide.find("do the 0.25 thing").expect("0.25.0 is in range");
        assert!(
            patch < spine && spine < last,
            "sections compose in ascending version order (0.22.10 < 0.23.0 < 0.25.0): {guide}"
        );
        assert!(guide.contains("### v0.23.0"), "{guide}");
        assert!(guide.contains("### v0.25.0"), "{guide}");
    }

    #[test]
    fn an_entry_without_a_migration_section_contributes_nothing() {
        // (0.23.0, 0.24.0] selects only v0.24.0, which carries no section.
        let guide = render_migration_guide(LOG, Some("0.23.0"), "0.24.0");
        assert!(guide.contains("No migration required"), "{guide}");
    }

    #[test]
    fn same_version_is_a_no_op() {
        let guide = render_migration_guide(LOG, Some("0.25.0"), "0.25.0");
        assert!(guide.contains("No migration required"), "{guide}");
    }

    #[test]
    fn a_malformed_old_version_lists_every_earlier_migration() {
        // Unparseable old → treated as the floor, so the guide errs toward
        // listing the migration rather than silently skipping it.
        let guide = render_migration_guide(LOG, Some("banana"), "0.25.0");
        for needle in [
            "patch-ordered task",
            "reverse the spine",
            "do the 0.25 thing",
        ] {
            assert!(guide.contains(needle), "{needle}: {guide}");
        }
    }

    #[test]
    fn the_shipped_changelog_slices_the_0_22_to_0_23_migration() {
        let guide = render_migration_guide(CHANGELOG, Some("0.22.2"), "0.23.0");
        for needle in [
            "realises",
            "depends-on",
            "drives",
            "represents",
            "attaches-to",
            "domain.actor",
            "solution.role",
            "is-role",
        ] {
            assert!(
                guide.contains(needle),
                "the shipped 0.23 migration: {needle}"
            );
        }
    }

    #[test]
    fn the_shipped_changelog_is_a_no_op_for_the_current_binary() {
        // A fresh init (no prior version) never needs a migration, whatever
        // the binary version is.
        let guide = render_migration_guide(CHANGELOG, None, env!("CARGO_PKG_VERSION"));
        assert!(guide.contains("No migration required"), "{guide}");
    }

    /// The nine shipped agent templates share one prompt shape — a closed
    /// frontmatter block, then a body carrying exactly the fixed six-section
    /// skeleton. Pinned over the embedded `AGENTS` bytes alone.
    #[test]
    fn shipped_agent_templates_carry_the_skeleton_and_body_budget() {
        const SKELETON: [&str; 5] = [
            "## Role",
            "## Permissions",
            "## Process",
            "## Hard limits",
            "## Report",
        ];
        // Each cap is the template's current body plus a small deliberate
        // margin, so a growth edit fails the cap and forces a re-pin rather
        // than silently eating slack. The corpus cap tracks the sum of these
        // caps plus 20: it flags aggregate re-pinning, while a single
        // template's growth is caught by its own cap.
        const CAPS: [(&str, usize); 9] = [
            ("agent-builder.md", 470),
            ("codebase-navigator.md", 454),
            ("implementation-phase-reviewer.md", 255),
            ("spec-writer.md", 168),
            ("plan-writer.md", 159),
            ("plan-review.md", 117),
            ("migration-agent.md", 88),
            ("spec-review.md", 81),
            ("researcher.md", 56),
        ];
        const CORPUS_CAP: usize = 1_868;

        /// The template's lines, without the empty element a trailing newline
        /// would split out (so the count matches the file's line count).
        fn lines_of(content: &str) -> Vec<&str> {
            content
                .strip_suffix('\n')
                .unwrap_or(content)
                .split('\n')
                .collect()
        }

        fn unquote(value: &str) -> &str {
            value
                .strip_prefix('"')
                .and_then(|inner| inner.strip_suffix('"'))
                .unwrap_or(value)
        }

        /// The `permissions:` value as ordered `{action, resource, effect}`
        /// rules: one `  - ` item, its keys at four spaces, in that order.
        fn permission_rules<'a>(
            name: &str,
            block: &'a [&'a str],
        ) -> Vec<(&'a str, &'a str, &'a str)> {
            let mut items: Vec<Vec<&'a str>> = Vec::new();
            for &line in block {
                if line.trim().is_empty() {
                    continue;
                }
                assert!(
                    line.starts_with("  "),
                    "{name}: only the indented `permissions:` sequence may follow that key: {line:?}"
                );
                if let Some(rest) = line.strip_prefix("  - ") {
                    items.push(vec![rest]);
                } else if let Some(rest) = line.strip_prefix("    ") {
                    items
                        .last_mut()
                        .unwrap_or_else(|| {
                            panic!("{name}: permission key without a rule: {line:?}")
                        })
                        .push(rest);
                } else {
                    panic!("{name}: malformed permission line: {line:?}");
                }
            }
            assert!(
                !items.is_empty(),
                "{name}: `permissions:` declares no rules"
            );
            items
                .into_iter()
                .map(|item| {
                    assert_eq!(
                        item.len(),
                        3,
                        "{name}: a permission rule is one {{action, resource, effect}} mapping: {item:?}"
                    );
                    (
                        permission_value(name, &item, 0, "action:"),
                        permission_value(name, &item, 1, "resource:"),
                        permission_value(name, &item, 2, "effect:"),
                    )
                })
                .collect()
        }

        fn permission_value<'a>(name: &str, rule: &[&'a str], index: usize, key: &str) -> &'a str {
            rule[index]
                .strip_prefix(key)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| {
                    panic!(
                        "{name}: permission rule `{key}` missing or empty: {:?}",
                        rule[index]
                    )
                })
        }

        assert_eq!(
            AGENTS.len(),
            CAPS.len(),
            "the shipped corpus is exactly the nine capped templates"
        );
        // The prune roster must mirror the installed one: `apg init` deletes a
        // `KNOWN_AGENT_FILES` name `AGENTS` no longer ships, so a stale name
        // would remove a live agent.
        assert_eq!(
            KNOWN_AGENT_FILES.len(),
            AGENTS.len(),
            "KNOWN_AGENT_FILES and AGENTS are both the nine shipped agents"
        );
        for &(name, _) in AGENTS {
            assert!(
                KNOWN_AGENT_FILES.contains(&name),
                "{name}: installed by AGENTS but missing from KNOWN_AGENT_FILES"
            );
        }
        for name in KNOWN_AGENT_FILES {
            assert!(
                AGENTS.iter().any(|(installed, _)| installed == name),
                "{name}: listed in KNOWN_AGENT_FILES but no longer installed"
            );
        }
        let mut total = 0;
        for &(name, content) in AGENTS {
            let cap = CAPS
                .iter()
                .find(|(template, _)| *template == name)
                .map(|(_, cap)| *cap)
                .unwrap_or_else(|| panic!("{name}: no body cap declared"));

            let lines = lines_of(content);
            assert_eq!(lines[0], "---", "{name}: the frontmatter opens on line 1");
            let close = lines
                .iter()
                .skip(1)
                .position(|line| *line == "---")
                .map(|index| index + 1)
                .unwrap_or_else(|| panic!("{name}: the frontmatter is never closed by `---`"));
            let frontmatter = &lines[1..close];
            let body = &lines[close + 1..];

            // (a) `permissions:` is the block's last top-level key, and only
            // its ordered sequence follows it.
            let last_key = frontmatter
                .iter()
                .rposition(|line| !line.is_empty() && !line.starts_with(' ') && line.contains(':'))
                .unwrap_or_else(|| panic!("{name}: the frontmatter declares no top-level key"));
            assert_eq!(
                frontmatter[last_key], "permissions:",
                "{name}: `permissions:` must be the frontmatter's last top-level key"
            );
            let rules = permission_rules(name, &frontmatter[last_key + 1..]);

            // The body: one H1 then exactly the five `##` sections, in order.
            let h1: Vec<&str> = body
                .iter()
                .copied()
                .filter(|line| line.starts_with("# "))
                .collect();
            assert_eq!(
                h1.len(),
                1,
                "{name}: the body carries exactly one H1: {h1:?}"
            );
            assert!(
                !h1[0][2..].trim().is_empty(),
                "{name}: the H1 names the agent"
            );
            assert_eq!(
                body.iter().find(|line| !line.trim().is_empty()).copied(),
                Some(h1[0]),
                "{name}: the H1 opens the body"
            );
            let headings: Vec<&str> = body
                .iter()
                .copied()
                .filter(|line| line.starts_with("## "))
                .collect();
            assert_eq!(
                headings, SKELETON,
                "{name}: the body carries exactly the six-section skeleton, in order"
            );

            // `## Permissions`: 4-6 lines of model explanation, then both lists.
            let section_at = body
                .iter()
                .position(|line| *line == "## Permissions")
                .unwrap_or_else(|| panic!("{name}: no `## Permissions` section"));
            let section_end = body[section_at + 1..]
                .iter()
                .position(|line| line.starts_with("## "))
                .map(|index| section_at + 1 + index)
                .unwrap_or(body.len());
            let section = &body[section_at + 1..section_end];
            let you_can = section
                .iter()
                .position(|line| *line == "You can:")
                .unwrap_or_else(|| panic!("{name}: `## Permissions` has no `You can:` list"));
            let you_cannot = section
                .iter()
                .position(|line| *line == "You cannot:")
                .unwrap_or_else(|| panic!("{name}: `## Permissions` has no `You cannot:` list"));
            assert!(
                you_can < you_cannot,
                "{name}: `You can:` must precede `You cannot:`"
            );
            let explanation = section[..you_can]
                .iter()
                .filter(|line| !line.trim().is_empty())
                .count();
            assert!(
                (4..=6).contains(&explanation),
                "{name}: the `## Permissions` model explanation is 4-6 lines, got {explanation}"
            );
            let can_entries = section[you_can + 1..you_cannot]
                .iter()
                .filter(|line| line.starts_with("- "))
                .count();
            let cannot_entries = section[you_cannot + 1..]
                .iter()
                .filter(|line| line.starts_with("- "))
                .count();
            assert!(
                can_entries > 0 && cannot_entries > 0,
                "{name}: both permission lists carry an entry ({can_entries} can, {cannot_entries} cannot)"
            );

            // (b) The shell rules: deny-all base, then allows; only
            // agent-builder carries the single env-prefixed narrow rule.
            let shell: Vec<(&str, &str, &str)> = rules
                .iter()
                .copied()
                .filter(|(action, _, _)| unquote(action) == "shell")
                .collect();
            assert!(
                !shell.is_empty(),
                "{name}: the frontmatter declares no shell rules"
            );
            let (_, resource, effect) = shell[0];
            assert!(
                unquote(resource) == "*" && unquote(effect) == "deny",
                "{name}: the shell rules open with a deny-all `*` rule, got {:?}",
                shell[0]
            );
            for (_, resource, effect) in &shell {
                assert!(
                    matches!(unquote(effect), "deny" | "allow"),
                    "{name}: shell rule `{}` has an unexpected effect {effect}",
                    unquote(resource)
                );
            }
            if name == "agent-builder.md" {
                let e2e: Vec<&(&str, &str, &str)> = shell
                    .iter()
                    .filter(|(_, resource, _)| unquote(resource).starts_with("APG_SUITE_E2E=1 "))
                    .collect();
                assert_eq!(
                    e2e.len(),
                    1,
                    "{name}: exactly one narrow `APG_SUITE_E2E=1` shell rule"
                );
                assert_eq!(
                    unquote(e2e[0].2),
                    "allow",
                    "{name}: the env-prefixed shell rule is an allow"
                );
            } else {
                assert!(
                    !shell
                        .iter()
                        .any(|(_, resource, _)| unquote(resource).starts_with("APG_SUITE_E2E=")),
                    "{name}: no shell rule may be env-prefixed with `APG_SUITE_E2E=`"
                );
            }

            // The retired edge-kind vocabulary, matched as backticked kind
            // tokens so the English verb "drives" never false-positives.
            for token in ["`drives`", "`realised-by`", "`depends-on`"] {
                assert!(
                    !content.contains(token),
                    "{name}: the retired edge-kind token {token} must be absent"
                );
            }

            total += body.len();
            assert!(
                body.len() <= cap,
                "{name}: the body is {} lines, over its {cap}-line cap",
                body.len()
            );
        }
        assert!(
            total <= CORPUS_CAP,
            "the nine bodies total {total} lines, over the {CORPUS_CAP}-line corpus cap"
        );
    }

    /// The research-escalation prose the five stage templates share: the
    /// request shape (the question, why it blocks, the decision the answer
    /// changes), the navigator's triage and fresh re-dispatch, and the writer's
    /// restatement of fetched content as its own sourced present-tense truth,
    /// never as untrusted durable truth. Pinned as substrings, so a reflow
    /// cannot fail it.
    #[test]
    fn research_escalation_prose_is_pinned_across_the_stage_templates() {
        fn template(name: &str) -> &'static str {
            AGENTS
                .iter()
                .find(|(installed, _)| *installed == name)
                .map(|(_, content)| *content)
                .unwrap_or_else(|| panic!("{name}: no shipped agent template with that name"))
        }

        fn assert_carries(
            name: &str,
            content: &str,
            needles: impl IntoIterator<Item = &'static str>,
        ) {
            for needle in needles {
                assert!(
                    content.contains(needle),
                    "`{name}` must carry the escalation prose {needle:?}"
                );
            }
        }

        const REQUEST: [&str; 3] = [
            "RESEARCH REQUEST: <question>",
            "why it blocks",
            "the decision the answer changes",
        ];
        // The writer side reads the returned markdown and restates it as its
        // own sourced present-tense truth; the scratch path is named.
        const WRITER: [&str; 4] = [
            "re-dispatches you fresh",
            "own sourced present-tense truth",
            "untrusted",
            "apg/.scratch/",
        ];
        // The reviewer side never rejects over the question and re-enters fresh.
        const REVIEWER: [&str; 3] = ["unrejected", "re-enters the review fresh", "untrusted"];
        // The navigator triages to one of three destinations and re-dispatches
        // the requesting stage fresh, holding the result as working material.
        const NAVIGATOR: [&str; 7] = [
            "triage",
            "three destinations",
            "exactly one target scratch file",
            "apg/.scratch/<subject>.md",
            "working material only",
            "untrusted",
            "Re-dispatch the requesting stage **fresh**",
        ];

        for name in ["spec-writer.md", "plan-writer.md"] {
            assert_carries(name, template(name), REQUEST.into_iter().chain(WRITER));
        }
        for name in ["spec-review.md", "plan-review.md"] {
            assert_carries(name, template(name), REQUEST.into_iter().chain(REVIEWER));
        }
        let navigator = "codebase-navigator.md";
        assert_carries(
            navigator,
            template(navigator),
            REQUEST.into_iter().chain(NAVIGATOR),
        );
    }
}
