//! Relocated e2e tests for the `structfrontend` crate (from `src/lib.rs`'s
//! inline `#[cfg(test)] mod tests`). Real I/O only; every test is
//! `#[ignore]`d and runs through the crate-local
//! `cargo test e2e:: -- --ignored`.

mod common;

use common::scratch_dir;
use common::structfrontend_bin;
use std::path::Path;

mod e2e {
    use super::*;

    #[test]
    #[ignore = "e2e tier: spawns the built structfrontend against a scratch /tmp dir; run via cargo test-e2e"]
    fn structfrontend_emits_claimed_files() {
        let dir = std::fs::canonicalize(scratch_dir("e2e")).expect("canonicalize scratch dir");
        let files: [(&str, &str); 9] = [
            ("build.sh", "build_all() {\n  echo hi\n}\n"),
            (
                "config.yaml",
                "name: CI\njobs:\n  build:\n    steps:\n      - run: make\n",
            ),
            ("data.json", "{\n  \"name\": \"x\"\n}\n"),
            ("Cargo.toml", "[package]\nname = \"x\"\n"),
            ("pom.xml", "<project>\n  <name>x</name>\n</project>\n"),
            ("Dockerfile", "FROM rust:1.98 AS builder\nRUN cargo build\n"),
            ("Makefile", "all:\n\techo hi\n"),
            ("app.ini", "[server]\nport = 8080\n"),
            ("README.md", "# Title\n\n## Section\n"),
        ];
        for (name, body) in files {
            std::fs::write(dir.join(name), body).expect("write fixture");
        }

        let out = std::process::Command::new(structfrontend_bin())
            .arg(&dir)
            .output()
            .expect("spawn structfrontend");
        assert!(
            out.status.success(),
            "structfrontend failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8(out.stdout).expect("utf8 stdout");
        let recs: Vec<serde_json::Value> = stdout
            .lines()
            .map(|l| serde_json::from_str(l).expect("jsonl record"))
            .collect();

        // One File record per claimed tracked file.
        let file_paths: Vec<&str> = recs
            .iter()
            .filter(|r| r["type"] == "file")
            .filter_map(|r| r["path"].as_str())
            .collect();
        assert_eq!(
            file_paths.len(),
            9,
            "one File record per claimed file: {file_paths:?}"
        );
        for (name, _) in files {
            let want = dir.join(name);
            assert!(
                file_paths.iter().any(|p| Path::new(p) == want.as_path()),
                "missing File record for {name}"
            );
        }

        // The repo-root module identity is the empty repo-relative identity
        // the ingestor renders as the bare `md.` root — never the checkout
        // basename.
        assert!(
            recs.iter().any(|r| r["type"] == "module" && r["fqn"] == ""),
            "root module identity must be empty: {recs:?}"
        );

        // The md heading Structs are unchanged: file-rooted parent + slugs.
        let readme = dir.join("README.md");
        let md_structs: Vec<&str> = recs
            .iter()
            .filter(|r| {
                r["type"] == "struct"
                    && r["path"]
                        .as_str()
                        .is_some_and(|p| Path::new(p) == readme.as_path())
            })
            .filter_map(|r| r["name"].as_str())
            .collect();
        assert_eq!(
            md_structs,
            vec!["title", "section"],
            "md heading Structs unchanged"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
