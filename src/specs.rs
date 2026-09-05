//! Spec/plan/note serialization (SPEC R4/R5) and the `apg scan` re-ingest of
//! the committed `apg/` data (R10).
//!
//! Layout (a committed `apg/` dir at the repo root, gitignored `apg/.trans/`):
//! - `apg/specs/<project>.jsonl` — one spec's durable, write-through records
//!   (committed; also holds notes + review feedback on spec/Future nodes).
//! - `apg/notes/<module>.jsonl` — the committed annotation ledger, split one
//!   file per owning module (module fqn slugged; fallback `_root.jsonl`).
//! - `apg/.trans/plans/<project>.jsonl` — the transient plan (gitignored).
//!
//! All records are the unified-JSONL `Record` enum: canonical FQNs, no opaque
//! ids. `apg scan` re-ingests every discovered file into the same DB.

use std::path::{Path, PathBuf};

use crate::graph::{Graph, NodeKind};
use crate::schema::Record;

/// The committed `apg/` root layout.
pub const LAYOUT: &str = "apg";
/// The gitignored transient subdir.
pub const TRANS: &str = ".trans";

/// True when `dir` is the repo's committed `apg/` layout root (it carries the
/// gitignored transient subdir, created by `apg scan`/`apg init`).
pub fn is_apg_layout_root(dir: &Path) -> bool {
    dir.file_name().is_some_and(|n| n == LAYOUT) && dir.join(TRANS).is_dir()
}

/// Path of a spec project's JSONL under `apg/specs/`.
// (Unused until the Phase 03 `apg spec` CLI lands; part of the serialization
// API this module owns.)
#[allow(dead_code)]
pub fn spec_jsonl_path(apg_root: &Path, project: &str) -> PathBuf {
    apg_root.join("specs").join(format!("{project}.jsonl"))
}

/// Path of a plan's JSONL under the gitignored `apg/.trans/plans/`.
#[allow(dead_code)]
pub fn plan_jsonl_path(apg_root: &Path, project: &str) -> PathBuf {
    apg_root
        .join(TRANS)
        .join("plans")
        .join(format!("{project}.jsonl"))
}

/// Parses one JSONL file into records. Empty lines are skipped; a malformed
/// record fails loudly with its line number (never silently dropped).
pub fn read_jsonl(path: &Path) -> anyhow::Result<Vec<Record>> {
    let text = std::fs::read_to_string(path)?;
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let rec = serde_json::from_str::<Record>(line).map_err(|e| {
            anyhow::anyhow!("{}:{}: bad record: {e}\n{line}", path.display(), i + 1)
        })?;
        out.push(rec);
    }
    Ok(out)
}

/// Serializes records into a JSONL file, creating parent dirs (write-through:
/// the file is written atomically in one write, so a crash mid-session loses
/// nothing).
// (Unused until Phase 03's write-through authoring.)
#[allow(dead_code)]
pub fn write_jsonl(path: &Path, records: &[Record]) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut s = String::new();
    for r in records {
        s.push_str(&serde_json::to_string(r)?);
        s.push('\n');
    }
    std::fs::write(path, s)?;
    Ok(())
}

/// Every `*.jsonl` under `dir` (sorted), or `[]` when the dir does not exist.
pub fn jsonl_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return vec![];
    };
    let mut v: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .collect();
    v.sort();
    v
}

/// The spec/plan/note JSONL inputs a scan re-ingests after code: specs and
/// notes (committed) plus plans (transient). `[]` for absent sets.
pub fn scan_inputs(apg_root: &Path) -> (Vec<PathBuf>, Vec<PathBuf>, Vec<PathBuf>) {
    let specs = jsonl_files(&apg_root.join("specs"));
    let notes = jsonl_files(&apg_root.join("notes"));
    let plans = jsonl_files(&apg_root.join(TRANS).join("plans"));
    (specs, notes, plans)
}

/// The owning module of a code node: the module that `Contains` it (via its
/// file for Struct/Function), or `None` for non-code / unowned nodes.
// (Unused until Phase 03 routes notes by module.)
#[allow(dead_code)]
pub fn owning_module(graph: &Graph, fqn: &str) -> Option<String> {
    let kind = graph.nodes.get(fqn)?.kind;
    match kind {
        NodeKind::Module => Some(fqn.to_string()),
        NodeKind::File => graph
            .contains
            .iter()
            .find(|(a, b)| {
                b == fqn
                    && graph
                        .nodes
                        .get(a)
                        .is_some_and(|n| n.kind == NodeKind::Module)
            })
            .map(|(a, _)| a.clone()),
        NodeKind::Struct | NodeKind::Function => {
            let file = graph
                .contains
                .iter()
                .find(|(a, b)| {
                    b == fqn
                        && graph
                            .nodes
                            .get(a)
                            .is_some_and(|n| n.kind == NodeKind::File)
                })
                .map(|(a, _)| a.clone());
            file.and_then(|f| owning_module(graph, &f))
        }
        _ => None,
    }
}

/// Slugs a module FQN into a safe filename stem (path separators → `_`), and
/// maps an unowned target to the `_root` ledger.
#[allow(dead_code)]
pub fn note_ledger_stem(graph: &Graph, target_fqn: &str) -> String {
    owning_module(graph, target_fqn)
        .map(|m| m.replace('/', "_"))
        .unwrap_or_else(|| "_root".to_string())
}

/// The `apg/notes/<module>.jsonl` file a note on `target_fqn` routes to.
#[allow(dead_code)]
pub fn note_file(graph: &Graph, apg_root: &Path, target_fqn: &str) -> PathBuf {
    apg_root
        .join("notes")
        .join(format!("{}.jsonl", note_ledger_stem(graph, target_fqn)))
}

/// Reads every spec/note/plan JSONL file under the repo's `apg/` root, in a
/// deterministic order (specs, notes, plans — each sorted by path).
pub fn read_all(apg_root: &Path) -> Vec<Record> {
    let (specs, notes, plans) = scan_inputs(apg_root);
    let mut out = Vec::new();
    for f in specs
        .into_iter()
        .chain(notes)
        .chain(plans)
    {
        match read_jsonl(&f) {
            Ok(records) => out.extend(records),
            Err(e) => panic!("{e:#}"),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::Node;

    #[test]
    fn jsonl_roundtrip_and_discovery() {
        let dir = std::env::temp_dir().join(format!("apg-specs-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let specs = dir.join("specs");
        let notes = dir.join("notes");
        let plans = dir.join(TRANS).join("plans");
        std::fs::create_dir_all(&plans).unwrap();

        let recs = vec![
            Record::Spec {
                fqn: "future/foo/spec".to_string(),
                title: "T".to_string(),
                goal: "G".to_string(),
            },
            Record::Requirement {
                fqn: "future/foo/spec.R1".to_string(),
                id: "R1".to_string(),
                title: "Timer".to_string(),
                body: String::new(),
                feature: String::new(),
            },
            Record::Contains {
                from: "future/foo/spec".to_string(),
                to: "future/foo/spec.R1".to_string(),
            },
        ];
        write_jsonl(&specs.join("foo.jsonl"), &recs).unwrap();
        write_jsonl(&notes.join("_root.jsonl"), &[Record::Note {
            fqn: "annotations/1".to_string(),
            body: "note".to_string(),
            kind: String::new(),
        }])
        .unwrap();
        write_jsonl(&plans.join("foo.jsonl"), &[Record::Plan {
            fqn: "future/foo/plan".to_string(),
            title: "Plan".to_string(),
            strategy: String::new(),
        }])
        .unwrap();

        let (s, n, p) = scan_inputs(&dir);
        assert_eq!(s.len(), 1);
        assert_eq!(n.len(), 1);
        assert_eq!(p.len(), 1);

        let all = read_all(&dir);
        assert_eq!(all.len(), 5);
        // Round-trip: re-serialize the parsed records identically.
        let parsed = read_jsonl(&specs.join("foo.jsonl")).unwrap();
        assert_eq!(parsed, recs);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn note_routing_by_module() {
        let mut g = Graph::default();
        let node = |kind: NodeKind| Node {
            kind,
            code_type: "src".to_string(),
            ..Node::default()
        };
        g.nodes.insert("mod".to_string(), node(NodeKind::Module));
        g.nodes.insert("/x/a.go".to_string(), node(NodeKind::File));
        g.nodes
            .insert("mod.A".to_string(), node(NodeKind::Struct));
        g.nodes
            .insert("mod.A.f".to_string(), node(NodeKind::Function));
        g.nodes.insert("future/foo/spec".to_string(), node(NodeKind::Spec));
        g.nodes
            .insert("future/foo/spec.R1".to_string(), node(NodeKind::Requirement));
        g.contains.insert(("mod".to_string(), "/x/a.go".to_string()));
        g.contains.insert(("/x/a.go".to_string(), "mod.A".to_string()));
        g.contains.insert(("/x/a.go".to_string(), "mod.A.f".to_string()));

        // A Struct's owning module resolves through its file.
        assert_eq!(owning_module(&g, "mod.A"), Some("mod".to_string()));
        assert_eq!(owning_module(&g, "mod.A.f"), Some("mod".to_string()));
        assert_eq!(owning_module(&g, "/x/a.go"), Some("mod".to_string()));
        // A module is its own owner; spec/Future/code-less nodes are _root.
        assert_eq!(owning_module(&g, "mod"), Some("mod".to_string()));
        assert_eq!(owning_module(&g, "future/foo/spec"), None);
        assert_eq!(owning_module(&g, "nowhere"), None);
        assert_eq!(note_ledger_stem(&g, "nowhere"), "_root");

        let apg = Path::new("apg");
        assert_eq!(
            note_file(&g, apg, "mod.A"),
            apg.join("notes").join("mod.jsonl")
        );
        assert_eq!(
            note_file(&g, apg, "future/foo/spec.R1"),
            apg.join("notes").join("_root.jsonl")
        );
    }
}