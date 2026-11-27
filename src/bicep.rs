//! The apg-side Bicep driver: the ingestor's bicep file-edge kinds and the
//! incremental target widening for the `bicep` stream.

use std::collections::BTreeSet;

/// Whether a `uses` edge's `properties` `kind` is a bicepfrontend Struct-to-File
/// kind (`module`, `using`, `extends`, `import`; exact, lowercase as the
/// frontend emits them). The ingestor rule: a `uses` edge whose kind is a
/// file-edge kind targets a File identity, not a symbol, so it is left unrooted
/// and kept Struct-to-File exactly as the ps `dot-source`/`import-module` edges.
pub fn is_file_edge_kind(kind: &str) -> bool {
    matches!(kind, "module" | "using" | "extends" | "import")
}

/// Widen an incremental re-emission target set (checkout-relative paths) to
/// every `bicep` file of `manifest` when it holds any `bicep` file; a set with
/// no `bicep` file is left untouched, and nothing is ever removed (non-bicep
/// targets and a deleted bicep path absent from the manifest stay).
///
/// Why: Bicep's facts depend on the whole template set — a `module` or `using`
/// edge reaches another file, and a `.bicepparam` file binds the template it
/// names — so a set holding any bicep file re-emits every bicep file, none is
/// reused from cached facts, and bicepfrontend receives the full bicep file list.
pub fn widen_targets(targets: &mut BTreeSet<String>, manifest: &crate::cache::Manifest) {
    let is_bicep = |p: &str| crate::incremental::language_of(p) == "bicep";
    if targets.iter().any(|t| is_bicep(t)) {
        targets.extend(manifest.entries.keys().filter(|k| is_bicep(k)).cloned());
    }
}
