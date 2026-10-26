//! Unit/int tests for the agent frontend: the tiers live in `tests::unit` and
//! `tests::int` beside the walk and stream units they exercise.

use crate::collect_definitions;

mod unit {
    use super::*;

    #[test]
    fn definition_stem_strips_the_extension_only() {
        let files = vec![
            (
                ".opencode/agents/go-frontend-implementer.md".to_string(),
                274,
            ),
            (".opencode/agents/extension-less".to_string(), 1),
            (
                ".opencode/agents/schema-alignment-plan-writer.md".to_string(),
                12,
            ),
        ];
        let defs = collect_definitions(&files);
        assert_eq!(
            defs,
            vec![
                (
                    ".opencode/agents/extension-less".to_string(),
                    "extension-less".to_string(),
                    1,
                    1,
                ),
                (
                    ".opencode/agents/go-frontend-implementer.md".to_string(),
                    "go-frontend-implementer".to_string(),
                    1,
                    274,
                ),
                (
                    ".opencode/agents/schema-alignment-plan-writer.md".to_string(),
                    "schema-alignment-plan-writer".to_string(),
                    1,
                    12,
                ),
            ]
        );
    }
}
