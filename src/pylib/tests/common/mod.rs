//! The `pyfrontend` crate-local shared e2e harness.
//!
//! Relocated verbatim from `src/lib.rs`'s inline `#[cfg(test)] mod tests`
//! root: the scratch-`/tmp` fixtures and JSONL record helpers every e2e test
//! in `tests/pylib_e2e.rs` reaches via `mod common;`. Each integration-test
//! binary includes this module separately, so items unused by a given binary
//! are expected.
#![allow(dead_code, unused_imports)]

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// The just-built `pyfrontend` beside the test executable's profile dir.
pub(super) fn frontend_bin() -> PathBuf {
    let exe = std::env::current_exe().expect("current exe");
    let profile_dir = exe
        .parent()
        .and_then(Path::parent)
        .expect("target profile dir")
        .to_path_buf();
    let unix = profile_dir.join("pyfrontend");
    if unix.is_file() {
        unix
    } else {
        profile_dir.join("pyfrontend.exe")
    }
}

pub(super) fn scratch_dir(name: &str) -> PathBuf {
    let mut dir = std::env::temp_dir();
    dir.push(format!("pyfrontend-e2e-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

pub(super) fn write_file(root: &Path, rel: &str, contents: &str) -> PathBuf {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create fixture dir");
    }
    std::fs::write(&path, contents).expect("write fixture");
    path
}

/// Runs the built frontend over `dir` with an EMPTY PATH: detection must be
/// marker-based and the frontend must never shell out to `python`/`uv`.
pub(super) fn run_frontend(dir: &Path, extra: &[&str]) -> String {
    let output = std::process::Command::new(frontend_bin())
        .arg(dir)
        .args(extra)
        .env("PATH", "")
        .output()
        .expect("spawn pyfrontend");
    assert!(
        output.status.success(),
        "pyfrontend failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

pub(super) fn records(stdout: &str) -> Vec<serde_json::Value> {
    stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("each stdout line is a JSON record"))
        .collect()
}

pub(super) fn module_fqns(recs: &[serde_json::Value]) -> HashSet<String> {
    recs.iter()
        .filter(|r| r["type"] == "module")
        .filter_map(|r| r["fqn"].as_str().map(str::to_string))
        .collect()
}

pub(super) fn node_id(
    recs: &[serde_json::Value],
    kind: &str,
    parent: &str,
    name: &str,
) -> Option<String> {
    recs.iter()
        .find(|r| r["type"] == kind && r["parent"] == parent && r["name"] == name)
        .and_then(|r| r["id"].as_str().map(str::to_string))
}

pub(super) fn has_edge(recs: &[serde_json::Value], kind: &str, from: &str, to: &str) -> bool {
    recs.iter()
        .any(|r| r["type"] == kind && r["from"] == from && r["to"] == to)
}

pub(super) fn edge_target(recs: &[serde_json::Value], kind: &str, from: &str) -> Option<String> {
    recs.iter()
        .find(|r| r["type"] == kind && r["from"] == from)
        .and_then(|r| r["to"].as_str().map(str::to_string))
}
