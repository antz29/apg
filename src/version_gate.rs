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
mod tests;
