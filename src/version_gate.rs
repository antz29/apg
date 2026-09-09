//! The layout version gate (apg-projects R9/R10): `apg/config.json` carries a
//! **binary-managed `version` field** — the version of the apg binary that
//! last initialized (or upgraded) the layout. The layout-touching operations
//! — `apg scan` and `apg project start` — refuse to run against a layout
//! whose declared version does not share the binary's major.minor, in EITHER
//! direction (an older binary must not reshape a newer layout any more than a
//! newer binary may silently rewrite an older one), and refuse a layout with
//! no declared version at all (a pre-versioning layout, or one never created
//! by `apg init`).
//!
//! The gate **blocks — it never warns**. `apg init` is the upgrade act: it
//! re-runs idempotently, writes the current version into `apg/config.json`
//! (the user's code_type rules untouched), and scaffolds the layout. Patch
//! versions never matter: `0.10.3` and `0.10.99` layouts are both fine for a
//! `0.10.4` binary. Full upgrade instructions ship as a suite doc
//! (`~/.opencode/lib/apg-upgrade.md`, installed by `apg init`); the block
//! text points at it.

use std::path::{Path, PathBuf};

/// The `config.json` field name of the binary-managed layout version.
pub const VERSION_FIELD: &str = "version";

/// The suite doc the block text points at (installed by `apg init` into
/// `~/.opencode/lib/` alongside the shared `apg.ts` plumbing).
pub const UPGRADE_DOC: &str = "~/.opencode/lib/apg-upgrade.md";

/// `apg/config.json` under a layout root.
fn config_path(apg_root: &Path) -> PathBuf {
    apg_root.join("config.json")
}

/// Why the version gate blocks (R10). Every variant maps to a refusal with
/// upgrade guidance; there is no warn-and-continue path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionBlock {
    /// `apg/config.json` exists but carries no binary-managed `version`
    /// field — a pre-versioning layout (or one `apg init` never wrote).
    Unversioned,
    /// The `version` field is present but does not parse as `X.Y[.Z…]`.
    Malformed { raw: String },
    /// Both versions parse; the major.minor pairs differ. The direction
    /// (layout older vs newer than the binary) is derived from the numbers —
    /// both directions block.
    Mismatch {
        layout_version: String,
        layout_major: u64,
        layout_minor: u64,
    },
}

/// The block-vs-proceed rules (R10) over parsed versions:
///
/// - `None` layout version (no field) → **block** (pre-versioning);
/// - an unparseable layout version → **block** (cannot tell which layout
///   format it declares);
/// - same major.minor as the binary (any patch, including a two-part
///   `X.Y`) → **proceed**;
/// - major or minor mismatch in either direction → **block**.
///
/// `binary_version` is the running binary's `CARGO_PKG_VERSION` (always
/// `X.Y.Z`); `layout_version` is the raw `version` field of
/// `apg/config.json`, or `None` when the field (or file) is absent.
pub fn check_version(
    binary_version: &str,
    layout_version: Option<&str>,
) -> Result<(), VersionBlock> {
    let Some(layout) = layout_version else {
        return Err(VersionBlock::Unversioned);
    };
    let Some((major, minor)) = parse_major_minor(layout) else {
        return Err(VersionBlock::Malformed {
            raw: layout.to_string(),
        });
    };
    let (bin_major, bin_minor) = parse_major_minor(binary_version)
        .expect("the binary version (CARGO_PKG_VERSION) is always X.Y.Z");
    if (bin_major, bin_minor) == (major, minor) {
        Ok(())
    } else {
        Err(VersionBlock::Mismatch {
            layout_version: layout.to_string(),
            layout_major: major,
            layout_minor: minor,
        })
    }
}

/// Parses the leading `X.Y` of a `X.Y[.Z…]` version (the gate never compares
/// patches). `None` for anything that is not at least two numeric
/// dot-separated components.
fn parse_major_minor(version: &str) -> Option<(u64, u64)> {
    let mut parts = version.trim().split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    Some((major, minor))
}

/// The R10 block text (the gate never warns): names the layout version vs
/// the binary, says which direction the mismatch runs, gives the upgrade act
/// (`apg init`, or a binary upgrade when the layout is newer), and points at
/// the suite doc. `retry` is the blocked operation's re-run line — e.g.
/// "re-run `apg scan`" or "re-run `apg project start <name>`".
pub fn block_message(
    block: &VersionBlock,
    binary_version: &str,
    path: &Path,
    retry: &str,
) -> String {
    let doc = format!(" See the upgrade guide at {UPGRADE_DOC}.");
    match block {
        VersionBlock::Unversioned => format!(
            "refused: {} declares no layout version — its binary-managed `version` field is missing (a pre-versioning layout, or one never created by `apg init`), and apg {binary_version} does not touch unversioned layouts. Fix: run `apg init` (writes the version; your code_type rules are untouched), then {retry}.{doc}",
            path.display()
        ),
        VersionBlock::Malformed { raw } => format!(
            "refused: {} declares layout version `{raw}`, which is not a valid version — apg {binary_version} cannot tell which layout format it is. Fix: run `apg init` (rewrites the binary-managed `version` field; your code_type rules are untouched), then {retry}.{doc}",
            path.display()
        ),
        VersionBlock::Mismatch {
            layout_version,
            layout_major,
            layout_minor,
        } => {
            let (bin_major, bin_minor) = parse_major_minor(binary_version)
                .expect("the binary version (CARGO_PKG_VERSION) is always X.Y.Z");
            if (*layout_major, *layout_minor) > (bin_major, bin_minor) {
                format!(
                    "refused: {} declares layout version {layout_version}, but this apg is {binary_version} — the layout's major.minor ({layout_major}.{layout_minor}) does not match the binary's ({bin_major}.{bin_minor}); the layout was written by a NEWER apg that this binary does not understand. Fix: upgrade apg to a version matching the layout's major.minor (e.g. `brew upgrade apg` or the install script), then re-run `apg init`, then {retry}.{doc}",
                    path.display()
                )
            } else {
                format!(
                    "refused: {} declares layout version {layout_version}, but this apg is {binary_version} — the layout's major.minor ({layout_major}.{layout_minor}) does not match the binary's ({bin_major}.{bin_minor}); the layout predates this apg line. Fix: upgrade the layout by re-running `apg init` (idempotent: writes the current version; your code_type rules are untouched), then {retry}.{doc}",
                    path.display()
                )
            }
        }
    }
}

/// Reads the layout's declared version: the `version` field of
/// `apg/config.json` at `apg_root` (`None` when the file or the field is
/// absent — the state the gate blocks).
pub fn config_version(apg_root: &Path) -> Option<String> {
    let text = std::fs::read_to_string(config_path(apg_root)).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value
        .get(VERSION_FIELD)
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

/// The R10 gate over a layout root — `apg scan` and `apg project start` call
/// this on the layout they are about to touch. Blocks (never warns) when the
/// layout's declared version is missing (unversioned layout, or no
/// `apg/config.json` at all) or its major.minor differs from the binary's in
/// either direction, with upgrade guidance naming the fix and pointing at the
/// suite doc. `retry` is the blocked op's re-run line.
pub fn require_layout_version(apg_root: &Path, retry: &str) -> anyhow::Result<()> {
    let binary = env!("CARGO_PKG_VERSION");
    let path = config_path(apg_root);
    if !path.exists() {
        anyhow::bail!(
            "refused: {} does not exist — apg {binary} only scans or starts projects against layouts initialized by `apg init` (init writes the binary-managed `version` field into apg/config.json and scaffolds apg/.worktrees/ + the .gitignore entries). Fix: run `apg init`, then {retry}. See the upgrade guide at {UPGRADE_DOC}.",
            path.display()
        );
    }
    match check_version(binary, config_version(apg_root).as_deref()) {
        Ok(()) => Ok(()),
        Err(block) => Err(anyhow::anyhow!(
            "{}",
            block_message(&block, binary, &path, retry)
        )),
    }
}

/// Writes the binary-managed `version` field of `apg/config.json` — the
/// layout's versioning act (`apg init` runs this on every init). The field
/// is set to `version` and the file rewritten, preserving every other field
/// (the user's code_type rules) untouched; a field that already equals
/// `version` leaves the file alone (idempotent — init re-runs are no-ops).
/// The file must exist and be a JSON object (init creates it first).
/// Returns whether the file was written.
pub fn ensure_config_version(apg_root: &Path, version: &str) -> anyhow::Result<bool> {
    let path = config_path(apg_root);
    let text = std::fs::read_to_string(&path)
        .map_err(|e| anyhow::anyhow!("cannot read {}: {e}", path.display()))?;
    let mut value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| anyhow::anyhow!("{} is not valid JSON: {e}", path.display()))?;
    let obj = value.as_object_mut().ok_or_else(|| {
        anyhow::anyhow!(
            "{} must be a JSON object — a scalar or array cannot carry the binary-managed `version` field",
            path.display()
        )
    })?;
    if obj.get(VERSION_FIELD).and_then(|v| v.as_str()) == Some(version) {
        return Ok(false);
    }
    obj.insert(
        VERSION_FIELD.to_string(),
        serde_json::Value::String(version.to_string()),
    );
    let mut out = serde_json::to_string_pretty(&value)?;
    out.push('\n');
    std::fs::write(&path, out)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixed binary version for the pure rule tests (they never depend on
    /// the crate's own version, so they survive release bumps).
    const BIN: &str = "0.10.4";

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
    // task-2/task-5 rules: same major.minor proceeds (patch diff fine);
    // missing blocks; major/minor mismatch in EITHER direction blocks.
    // ------------------------------------------------------------------

    #[test]
    fn same_major_minor_proceeds_any_patch() {
        for v in ["0.10.0", "0.10.3", "0.10.4", "0.10.99"] {
            assert_eq!(check_version(BIN, Some(v)), Ok(()), "layout {v}");
        }
        // Two-part versions (no patch component) parse and compare too.
        assert_eq!(check_version(BIN, Some("0.10")), Ok(()));
    }

    #[test]
    fn missing_version_blocks() {
        assert_eq!(check_version(BIN, None), Err(VersionBlock::Unversioned));
    }

    #[test]
    fn malformed_version_blocks() {
        for v in ["", "banana", "0", "v0.10.4", "0.10-beta", "0..4"] {
            assert_eq!(
                check_version(BIN, Some(v)),
                Err(VersionBlock::Malformed { raw: v.to_string() }),
                "layout `{v}` must be malformed"
            );
        }
    }

    #[test]
    fn older_major_or_minor_blocks() {
        for v in ["0.9.0", "0.9.12", "0.0.1", "0.0.0"] {
            assert!(
                matches!(
                    check_version(BIN, Some(v)),
                    Err(VersionBlock::Mismatch { .. })
                ),
                "layout {v} must block (older)"
            );
        }
    }

    #[test]
    fn newer_major_or_minor_blocks() {
        for v in ["0.11.0", "0.11.4", "1.0.0", "2.3.4"] {
            assert!(
                matches!(
                    check_version(BIN, Some(v)),
                    Err(VersionBlock::Mismatch { .. })
                ),
                "layout {v} must block (newer)"
            );
        }
    }

    #[test]
    fn mismatch_carries_the_layout_pair_for_direction() {
        match check_version(BIN, Some("0.9.7")) {
            Err(VersionBlock::Mismatch {
                layout_version,
                layout_major,
                layout_minor,
            }) => {
                assert_eq!(layout_version, "0.9.7");
                assert_eq!((layout_major, layout_minor), (0, 9));
            }
            other => panic!("expected mismatch, got {other:?}"),
        }
    }

    // ------------------------------------------------------------------
    // Block text (R10): upgrade guidance in both directions + doc pointer
    // ------------------------------------------------------------------

    #[test]
    fn block_messages_carry_direction_fix_and_doc_pointer() {
        let path = Path::new("/repo/apg/config.json");
        // Older layout: upgrade the layout via init.
        let older = block_message(
            &VersionBlock::Mismatch {
                layout_version: "0.9.7".into(),
                layout_major: 0,
                layout_minor: 9,
            },
            BIN,
            path,
            "re-run `apg scan`",
        );
        for needle in [
            "0.9.7",
            BIN,
            "0.9",
            "0.10",
            "apg init",
            "re-run `apg scan`",
            UPGRADE_DOC,
        ] {
            assert!(older.contains(needle), "older block text: {older}");
        }
        // Newer layout: upgrade the binary, then init.
        let newer = block_message(
            &VersionBlock::Mismatch {
                layout_version: "1.0.0".into(),
                layout_major: 1,
                layout_minor: 0,
            },
            BIN,
            path,
            "re-run `apg project start foo`",
        );
        for needle in [
            "1.0.0",
            "NEWER apg",
            "upgrade apg",
            "apg init",
            "re-run `apg project start foo`",
        ] {
            assert!(newer.contains(needle), "newer block text: {newer}");
        }
        // Unversioned: init guidance.
        let unversioned = block_message(&VersionBlock::Unversioned, BIN, path, "re-run `apg scan`");
        for needle in [
            "no layout version",
            "apg init",
            "re-run `apg scan`",
            UPGRADE_DOC,
        ] {
            assert!(
                unversioned.contains(needle),
                "unversioned block text: {unversioned}"
            );
        }
    }

    // ------------------------------------------------------------------
    // Config read + the init version write (task-1: binary-managed field,
    // user code_type rules untouched)
    // ------------------------------------------------------------------

    #[test]
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
