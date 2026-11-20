//! The apg-side PowerShell driver: the ingestor's ps file-edge kinds and the
//! incremental target widening for the `ps` stream.

/// Whether a `uses` edge's `properties` `kind` is a psfrontend Struct-to-File
/// kind (`dot-source`, `import-module`; exact, lowercase as
/// `psfrontend::resolve::DepKind::as_str` emits them). The ingestor rule: a
/// `uses` edge whose kind is a file-edge kind targets a File identity, not a
/// symbol, so it is left unrooted and kept Struct-to-File exactly as the COBOL
/// `copy` edge (which stays the ingestor's own literal, not a kind here).
pub fn is_file_edge_kind(kind: &str) -> bool {
    matches!(kind, "dot-source" | "import-module")
}
