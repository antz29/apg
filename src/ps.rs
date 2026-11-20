//! The apg-side PowerShell driver: the ingestor's ps file-edge kinds and the
//! incremental target widening for the `ps` stream.

use std::collections::BTreeSet;

/// Whether a `uses` edge's `properties` `kind` is a psfrontend Struct-to-File
/// kind (`dot-source`, `import-module`; exact, lowercase as
/// `psfrontend::resolve::DepKind::as_str` emits them). The ingestor rule: a
/// `uses` edge whose kind is a file-edge kind targets a File identity, not a
/// symbol, so it is left unrooted and kept Struct-to-File exactly as the COBOL
/// `copy` edge (which stays the ingestor's own literal, not a kind here).
pub fn is_file_edge_kind(kind: &str) -> bool {
    matches!(kind, "dot-source" | "import-module")
}

/// Widen an incremental re-emission target set (checkout-relative paths) to
/// every `ps` file of `manifest` when it holds any `ps` file; a set with no
/// `ps` file is left untouched, and nothing is ever removed (non-ps targets
/// and a deleted ps path absent from the manifest stay).
///
/// Why: PowerShell FQNs and command resolution depend on the whole claimed
/// tree — cross-file collisions and `//path:ordinal` suffixes, the callers of
/// a renamed or re-suffixed unit, and the global export lookup — so a set
/// holding any ps file re-emits every ps file, none is reused from cached
/// facts, and psfrontend (which parses the whole claimed tree each run)
/// receives the full ps file list as `--targets`.
pub fn widen_targets(targets: &mut BTreeSet<String>, manifest: &crate::cache::Manifest) {
    let is_ps = |p: &str| crate::incremental::language_of(p) == "ps";
    if targets.iter().any(|t| is_ps(t)) {
        targets.extend(manifest.entries.keys().filter(|k| is_ps(k)).cloned());
    }
}
