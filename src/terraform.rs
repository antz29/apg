//! The apg-side Terraform driver: the ingestor's terraform file-edge kinds and
//! the incremental target widening for the `terraform` stream.

use std::collections::BTreeSet;

/// Whether a `uses` edge's `properties` `kind` is a terraformfrontend
/// Struct-to-File kind (`module-source`, `override`; exact, lowercase as the
/// frontend emits them). The ingestor rule: a `uses` edge whose kind is a
/// file-edge kind targets a File identity, not a symbol, so it is left unrooted
/// and kept Struct-to-File exactly as the ps `dot-source`/`import-module` edges.
pub fn is_file_edge_kind(kind: &str) -> bool {
    matches!(kind, "module-source" | "override")
}

/// Widen an incremental re-emission target set (checkout-relative paths) to
/// every `terraform` file of `manifest` when it holds any `terraform` file; a
/// set with no `terraform` file is left untouched, and nothing is ever removed
/// (non-terraform targets and a deleted terraform path absent from the manifest
/// stay).
///
/// Why: Terraform's facts depend on the whole module, not one file — sibling
/// files share one namespace, `override` files replace declarations in their
/// siblings, `.tofu` files shadow `.tf` files of the same name, and a
/// `module-source` edge reaches another directory's module — so a set holding
/// any terraform file re-emits every terraform file, none is reused from cached
/// facts, and terraformfrontend receives the full terraform file list.
pub fn widen_targets(targets: &mut BTreeSet<String>, manifest: &crate::cache::Manifest) {
    let is_tf = |p: &str| crate::incremental::language_of(p) == "terraform";
    if targets.iter().any(|t| is_tf(t)) {
        targets.extend(manifest.entries.keys().filter(|k| is_tf(k)).cloned());
    }
}
