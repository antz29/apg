//! The root crate's shared e2e harness.
//!
//! Each integration-test binary in `tests/` (a `tests/*_e2e.rs` crate)
//! includes this module separately via `mod common;`, so items unused by a
//! given binary are expected.
#![allow(dead_code, unused_imports)]

// Keep this thin: the fixtures are single-sourced on the public library
// harness `apg::testutil` (the exactly-one-definition home for `with_cwd`,
// `wt_commit` and `wt_commit_paths`), and this module only re-exports them to
// the root e2e crates. Module-specific helpers belong in that module's own
// e2e file, never here.
pub use apg::testutil::{
    ApgCommand, CWD_LOCK, Repo, SessionProcess, apg_bin, code_payload, commit_count, copy_dir,
    file_line, function_line, git2_repo, module_line, payload_files, project_with_db, remove,
    scan_checkout, scan_checkout_locked, spawn_apg, start_session_process, struct_line, touch_db,
    with_cwd, write_scan_meta, write_scan_meta_keyed, wt_commit, wt_commit_paths,
};
