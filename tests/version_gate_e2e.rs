mod common;

use apg::version_gate::*;
use std::path::{Path, PathBuf};

fn tmp_apg_root(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("apg-vg-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn write_config(apg_root: &Path, json: &str) {
    std::fs::write(apg_root.join("config.json"), json).unwrap();
}

// ------------------------------------------------------------------
// Config read + the init version write (task-1: binary-managed field,
// user code_type rules untouched)
// ------------------------------------------------------------------

/// e2e tier -- real I/O: these tests create temp roots and read/write
/// `apg/config.json` on disk. Each is `#[ignore]`d, so a plain `cargo test`
/// never runs one; the only entry point is the named guard `cargo test-e2e`
/// (= `cargo test tests::e2e:: -- --ignored`).
mod e2e {
    use super::*;

    #[test]
    #[ignore = "e2e tier: real I/O (apg/config.json on disk); run via cargo test-e2e"]
    fn config_version_reads_the_field() {
        let root = tmp_apg_root("read");
        write_config(
            &root,
            "{\n  \"default\": \"src\",\n  \"version\": \"0.10.4\"\n}\n",
        );
        assert_eq!(config_version(&root).as_deref(), Some("0.10.4"));
        // No version field → None (the state the gate blocks).
        write_config(&root, "{ \"default\": \"src\", \"types\": [] }\n");
        assert_eq!(config_version(&root), None);
        // No config at all → None.
        std::fs::remove_file(root.join("config.json")).unwrap();
        assert_eq!(config_version(&root), None);
        // Unparseable JSON → None (cannot trust the file).
        write_config(&root, "{ not json");
        assert_eq!(config_version(&root), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (apg/config.json on disk); run via cargo test-e2e"]
    fn ensure_config_version_writes_field_preserving_rules() {
        let root = tmp_apg_root("ensure");
        write_config(
            &root,
            "{\"default\":\"test\",\"types\":[{\"name\":\"generated\",\"globs\":[\"**/*.pb.go\",\"**/gen/**\"]}]}\n",
        );
        assert!(ensure_config_version(&root, "0.10.4").unwrap());
        let out = std::fs::read_to_string(root.join("config.json")).unwrap();
        let value: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(value["version"], "0.10.4");
        assert_eq!(value["default"], "test", "user default untouched");
        assert_eq!(
            value["types"][0]["globs"][1], "**/gen/**",
            "user code_type rules untouched"
        );
        // Idempotent: a matching version leaves the file alone.
        assert!(!ensure_config_version(&root, "0.10.4").unwrap());
        let again = std::fs::read_to_string(root.join("config.json")).unwrap();
        assert_eq!(out, again);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (apg/config.json on disk); run via cargo test-e2e"]
    fn ensure_config_version_updates_a_stale_version() {
        let root = tmp_apg_root("update");
        write_config(
            &root,
            "{ \"default\": \"src\", \"types\": [], \"version\": \"0.10.3\" }\n",
        );
        assert!(ensure_config_version(&root, "0.10.4").unwrap());
        assert_eq!(config_version(&root).as_deref(), Some("0.10.4"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (apg/config.json on disk); run via cargo test-e2e"]
    fn ensure_config_version_refuses_malformed_config() {
        let root = tmp_apg_root("bad");
        write_config(&root, "[1, 2, 3]");
        let err = ensure_config_version(&root, "0.10.4").unwrap_err();
        assert!(format!("{err:#}").contains("JSON object"), "{err:#}");
        let _ = std::fs::remove_dir_all(&root);
    }

    // ------------------------------------------------------------------
    // The root gate (task-3): missing file, missing field, patch diff
    // proceeds, mismatch blocks with guidance — in both directions.
    // ------------------------------------------------------------------

    #[test]
    #[ignore = "e2e tier: real I/O (apg/config.json on disk); run via cargo test-e2e"]
    fn gate_blocks_when_config_file_is_absent() {
        let root = tmp_apg_root("nofile");
        let err = require_layout_version(&root, "re-run `apg scan`").unwrap_err();
        let msg = format!("{err:#}");
        for needle in [
            "does not exist",
            "apg init",
            "re-run `apg scan`",
            UPGRADE_DOC,
        ] {
            assert!(msg.contains(needle), "{msg}");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (apg/config.json on disk); run via cargo test-e2e"]
    fn gate_blocks_unversioned_layout() {
        let root = tmp_apg_root("unversioned");
        write_config(&root, "{ \"default\": \"src\", \"types\": [] }\n");
        let err = require_layout_version(&root, "re-run `apg scan`").unwrap_err();
        let msg = format!("{err:#}");
        for needle in [
            "no layout version",
            "apg init",
            "re-run `apg scan`",
            UPGRADE_DOC,
        ] {
            assert!(msg.contains(needle), "{msg}");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (apg/config.json on disk); run via cargo test-e2e"]
    fn gate_blocks_mismatch_in_both_directions() {
        let root = tmp_apg_root("mismatch");
        for (layout, needle) in [("0.9.0", "predates"), ("1.0.0", "NEWER apg")] {
            write_config(
                &root,
                &format!("{{ \"default\": \"src\", \"version\": \"{layout}\" }}\n"),
            );
            let err = require_layout_version(&root, "re-run `apg scan`").unwrap_err();
            let msg = format!("{err:#}");
            assert!(msg.contains(layout), "{msg}");
            assert!(msg.contains(needle), "{layout}: {msg}");
            assert!(msg.contains("apg init"), "{msg}");
            assert!(msg.contains(UPGRADE_DOC), "{msg}");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    #[ignore = "e2e tier: real I/O (apg/config.json on disk); run via cargo test-e2e"]
    fn gate_passes_for_patch_diff_and_current_version() {
        let root = tmp_apg_root("pass");
        // Patch-only differences never block (task-5: 0.10.3 vs a 0.10.4
        // binary). Versions are derived from the crate's own so the test
        // survives release bumps: same major.minor always proceeds.
        let v: Vec<u64> = env!("CARGO_PKG_VERSION")
            .split('.')
            .map(|p| p.parse().unwrap())
            .collect();
        let patch_shifted = format!("{}.{}.{}", v[0], v[1], v[2] + 1);
        write_config(
            &root,
            &format!("{{ \"default\": \"src\", \"version\": \"{patch_shifted}\" }}\n"),
        );
        require_layout_version(&root, "re-run `apg scan`").unwrap();
        write_config(
            &root,
            &format!(
                "{{ \"default\": \"src\", \"version\": \"{}\" }}\n",
                env!("CARGO_PKG_VERSION")
            ),
        );
        require_layout_version(&root, "re-run `apg scan`").unwrap();
        let _ = std::fs::remove_dir_all(&root);
    }
}
