//! The `structfrontend` crate's shared e2e harness.
//!
//! Each integration-test binary in `tests/` (a `tests/*_e2e.rs` crate)
//! includes this module separately via `mod common;`, so items unused by a
//! given binary are expected.
#![allow(dead_code)]

use std::path::PathBuf;

/// A fresh scratch directory for the e2e tier.
pub fn scratch_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("structfrontend-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

/// Locates the built `structfrontend` binary: the compile-time
/// `CARGO_BIN_EXE_structfrontend` when Cargo provided it, else the
/// `structfrontend` sibling of the profile dir (walking up from the test
/// harness executable) — the artifact `cargo build` leaves.
pub fn structfrontend_bin() -> PathBuf {
    if let Some(p) = option_env!("CARGO_BIN_EXE_structfrontend") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return p;
        }
    }
    let exe = std::env::current_exe().expect("current_exe");
    let name = if cfg!(windows) {
        "structfrontend.exe"
    } else {
        "structfrontend"
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
        "could not locate the built `structfrontend` binary from {} — run \
         `cargo build --manifest-path src/structlib/Cargo.toml` first",
        exe.display()
    );
}
