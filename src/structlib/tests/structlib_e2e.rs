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

    #[test]
    #[ignore = "e2e tier: spawns the built structfrontend against a scratch /tmp tree; run via cargo test-e2e"]
    fn structfrontend_streams_msbuild_as_xml_and_sln_as_sln() {
        let dir = std::fs::canonicalize(scratch_dir("msbuild")).expect("canonicalize scratch dir");
        let msbuild = [
            "App.csproj",
            "Directory.Build.props",
            "Custom.targets",
            "app.config",
            "Strings.resx",
        ];
        for name in msbuild {
            std::fs::write(dir.join(name), "<Project>\n  <A>x</A>\n</Project>\n")
                .expect("write msbuild fixture");
        }
        std::fs::write(
            dir.join("App.sln"),
            "Microsoft Visual Studio Solution File, Format Version 12.00\n\
             Project(\"{FAE04EC0-301F-11D3-BF4B-00C04F79EFBC}\") = \"App\", \"App.csproj\", \"{11111111-1111-1111-1111-111111111111}\"\n\
             EndProject\n\
             Global\n\
             \tGlobalSection(SolutionConfigurationPlatforms) = preSolution\n\
             \t\tDebug|Any CPU = Debug|Any CPU\n\
             \tEndGlobalSection\n\
             EndGlobal\n",
        )
        .expect("write sln fixture");

        // One spawn per stream, selected the way the scan driver does.
        let stream = |id: &str| -> Vec<serde_json::Value> {
            let out = std::process::Command::new(structfrontend_bin())
                .arg(&dir)
                .args(["--stream", id])
                .output()
                .expect("spawn structfrontend");
            assert!(
                out.status.success(),
                "structfrontend --stream {id} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8(out.stdout)
                .expect("utf8 stdout")
                .lines()
                .map(|l| serde_json::from_str(l).expect("jsonl record"))
                .collect()
        };
        let file_names = |recs: &[serde_json::Value]| -> Vec<String> {
            let mut names: Vec<String> = recs
                .iter()
                .filter(|r| r["type"] == "file")
                .filter_map(|r| r["path"].as_str())
                .map(|p| {
                    Path::new(p)
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .into_owned()
                })
                .collect();
            names.sort();
            names
        };

        // The xml stream claims exactly the MSBuild/config/resource files.
        let xml = stream("xml");
        let mut want: Vec<String> = msbuild.iter().map(|s| s.to_string()).collect();
        want.sort();
        assert_eq!(file_names(&xml), want, "xml stream files: {xml:?}");

        // The sln stream claims the .sln alone, with its Project and
        // GlobalSection structs.
        let sln = stream("sln");
        assert_eq!(
            file_names(&sln),
            vec!["App.sln"],
            "sln stream files: {sln:?}"
        );
        let structs: Vec<&str> = sln
            .iter()
            .filter(|r| r["type"] == "struct")
            .filter_map(|r| r["name"].as_str())
            .collect();
        assert_eq!(
            structs,
            vec!["App", "SolutionConfigurationPlatforms"],
            "sln Project + GlobalSection structs"
        );

        // Neither lands in the residual misc stream.
        assert!(
            file_names(&stream("misc")).is_empty(),
            "nothing left to misc"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    #[ignore = "e2e tier: spawns the built structfrontend against a scratch /tmp tree; run via cargo test-e2e"]
    fn structfrontend_prunes_top_level_apg_layout() {
        let dir =
            std::fs::canonicalize(scratch_dir("apg-layout")).expect("canonicalize scratch dir");
        let layout = dir.join("apg");
        std::fs::create_dir_all(layout.join("layers/requirements/requirement"))
            .expect("create apg layout");
        std::fs::write(layout.join("config.json"), "{\n  \"default\": \"src\"\n}\n")
            .expect("write layout config");
        std::fs::write(
            layout.join("layers/requirements/requirement/req.json"),
            "{\n  \"layer\": \"requirements\"\n}\n",
        )
        .expect("write layout node");
        std::fs::write(dir.join("README.md"), "# Title\n").expect("write root fixture");

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

        // The scan ran: the root fixture is still claimed.
        let readme = dir.join("README.md");
        assert!(
            recs.iter().any(|r| {
                r["type"] == "file"
                    && r["path"]
                        .as_str()
                        .is_some_and(|p| Path::new(p) == readme.as_path())
            }),
            "root README.md missing from {recs:?}"
        );

        // No File record originates under the top-level apg/ layout.
        let stray_files: Vec<&str> = recs
            .iter()
            .filter(|r| r["type"] == "file")
            .filter_map(|r| r["path"].as_str())
            .filter(|p| Path::new(p).starts_with(layout.as_path()))
            .collect();
        assert!(
            stray_files.is_empty(),
            "File records leaked from the top-level apg/ layout: {stray_files:?}"
        );

        // No Module identity is the top-level apg/ directory or anything under it.
        let stray_modules: Vec<&str> = recs
            .iter()
            .filter(|r| r["type"] == "module")
            .filter_map(|r| r["fqn"].as_str())
            .filter(|fqn| *fqn == "apg" || fqn.starts_with("apg/"))
            .collect();
        assert!(
            stray_modules.is_empty(),
            "Module records leaked from the top-level apg/ layout: {stray_modules:?}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
