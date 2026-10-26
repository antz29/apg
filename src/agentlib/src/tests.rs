//! Unit/int tests for the agent frontend: the tiers live in `tests::unit` and
//! `tests::int` beside the walk and stream units they exercise.

use crate::{collect_definitions, emit_stream};

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

mod int {
    use super::*;

    #[test]
    fn walk_and_stream_claim_only_agent_definitions() {
        let files = vec![
            (
                ".opencode/agents/go-frontend-implementer.md".to_string(),
                274,
            ),
            (
                ".opencode/agents/schema-alignment-plan-writer.md".to_string(),
                12,
            ),
            // Paths outside the claim scope: never a definition.
            (".opencode/agents.md".to_string(), 3),
            (".opencode/README.md".to_string(), 8),
            ("docs/README.md".to_string(), 40),
            (".opencode/agents/nested/notes.md".to_string(), 5),
        ];

        // The walk alone claims only the directory's direct children.
        let defs = collect_definitions(&files);
        assert_eq!(
            defs.iter()
                .map(|(path, stem, _, _)| (path.as_str(), stem.as_str()))
                .collect::<Vec<_>>(),
            vec![
                (
                    ".opencode/agents/go-frontend-implementer.md",
                    "go-frontend-implementer",
                ),
                (
                    ".opencode/agents/schema-alignment-plan-writer.md",
                    "schema-alignment-plan-writer",
                ),
            ]
        );

        // Wire the walk to the stream: one directory module, then per kept
        // definition one parented file record and one stem-named agent record.
        let mut out = Vec::new();
        emit_stream(&mut out, &defs).unwrap();
        let text = String::from_utf8(out).unwrap();
        let records: Vec<serde_json::Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();

        let modules: Vec<_> = records.iter().filter(|r| r["type"] == "module").collect();
        assert_eq!(modules.len(), 1);
        assert_eq!(modules[0]["fqn"], ".opencode/agents");

        let streamed_files: Vec<_> = records.iter().filter(|r| r["type"] == "file").collect();
        let agents: Vec<_> = records.iter().filter(|r| r["type"] == "agent").collect();
        assert_eq!(streamed_files.len(), defs.len());
        assert_eq!(agents.len(), defs.len());
        assert_eq!(records.len(), 1 + 2 * defs.len());

        for (path, stem, start_line, end_line) in &defs {
            let file = streamed_files
                .iter()
                .find(|r| r["path"] == path.as_str())
                .expect("file record per kept definition");
            assert_eq!(file["parent"], ".opencode/agents");
            assert_eq!(file["start_line"], *start_line);
            assert_eq!(file["end_line"], *end_line);

            let agent = agents
                .iter()
                .find(|r| r["path"] == path.as_str())
                .expect("agent record per kept definition");
            assert_eq!(agent["name"], stem.as_str());
            assert_eq!(agent["start_line"], *start_line);
            assert_eq!(agent["end_line"], *end_line);
        }

        // Exactly one agent record per definition stem identity.
        let mut names: Vec<&str> = agents.iter().map(|a| a["name"].as_str().unwrap()).collect();
        names.sort_unstable();
        let mut stems: Vec<&str> = defs.iter().map(|(_, stem, _, _)| stem.as_str()).collect();
        stems.sort_unstable();
        assert_eq!(names, stems);
    }
}
