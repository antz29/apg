//! The `pyfrontend` crate-local shared e2e harness.
//!
//! The scratch-`/tmp` fixtures and JSONL record helpers every e2e test
//! in `tests/pylib_e2e.rs` reaches via `mod common;`. Each integration-test
//! binary includes this module separately, so items unused by a given binary
//! are expected.
#![allow(dead_code)]

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Locates the built `pyfrontend` binary: the compile-time
/// `CARGO_BIN_EXE_pyfrontend` when Cargo provided it, else the first
/// `pyfrontend` found walking up from the test harness executable — the
/// artifact `cargo build` leaves in the profile dir.
pub(super) fn frontend_bin() -> PathBuf {
    if let Some(p) = option_env!("CARGO_BIN_EXE_pyfrontend") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return p;
        }
    }
    let exe = std::env::current_exe().expect("current exe");
    let name = if cfg!(windows) {
        "pyfrontend.exe"
    } else {
        "pyfrontend"
    };
    let mut dir = exe.parent();
    while let Some(d) = dir {
        let candidate = d.join(name);
        if candidate.is_file() {
            return candidate;
        }
        dir = d.parent();
    }
    panic!(
        "could not locate the built `pyfrontend` binary from {} — run \
         `cargo build --manifest-path src/pylib/Cargo.toml` first",
        exe.display()
    );
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
