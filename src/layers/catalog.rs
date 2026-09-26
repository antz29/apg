//! The layer catalog and layout constants (SPEC §3.1/§4.1/§5): the six
//! logical layers, the node types each may hold, where each layer's authored
//! nodes serialize, and the durable/transient tree tables.

/// The durable node-file root: `layers/` under the layout root (SPEC §4.1) —
/// one file per node at `<layer>/<type>/<name>.json`, the file name IS the
/// identity.
// (Unused until the phase-3 write/ingest tasks join this onto a layout root.)
#[allow(dead_code)]
pub const LAYERS_DIR: &str = "layers";

/// The gitignored transient root: `.trans/` under the layout root (SPEC
/// §4.1/§5) — the per-branch plan store plus the transient mirrors of the
/// layer tree.
// (Unused until the phase-3 write/ingest tasks join this onto a layout root.)
#[allow(dead_code)]
pub const TRANS_DIR: &str = ".trans";

/// Where a layer's authored nodes serialize (SPEC §3.1, storage policy —
/// separate from the catalog itself).
// (Unused until the phase-3 write/ingest tasks consult the catalog.)
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoragePolicy {
    /// Node files under the committed `apg/layers/<layer>/` tree — durable;
    /// a file's present-ness on a branch is the node's present-ness.
    Durable,
    /// Only under the gitignored `apg/.trans/plans/` — transient, per
    /// branch, never committed. The plans layer's whole serialization.
    TransientPlans,
    /// The layer's real nodes are the scanned code — never serialized; only
    /// its attach-only node types (note, constraint) have durable files under
    /// `apg/layers/<layer>/`.
    ScannedCode,
}

/// The six logical layers of the catalog (SPEC §3.1). The catalog is
/// layer-scoped; [`Layer::storage`] carries the storage policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Layer {
    /// Tier 1 — the stakeholder/requirement hierarchy (why).
    Requirements,
    /// Tier 2 — DDD semantics with plain names (what).
    Domain,
    /// Tier 3 — C4 only (how).
    Solution,
    /// Tier 4 — the bridge: plan phases, tasks, planned Implementation nodes.
    Plans,
    /// Tier 5 — the code in the branch (scanned, never serialized).
    Implementation,
    /// The laws (constraints over the whole graph) and notes on them.
    Global,
}

// (Unused until the phase-3/4 write/ingest tasks consult the catalog.)
#[allow(dead_code)]
impl Layer {
    /// All six layers in catalog order: requirements, domain, solution,
    /// plans, implementation, global.
    pub const ALL: [Layer; 6] = [
        Layer::Requirements,
        Layer::Domain,
        Layer::Solution,
        Layer::Plans,
        Layer::Implementation,
        Layer::Global,
    ];

    /// The layer's directory name — the dir under `apg/layers/` for the
    /// file-backed layers, under `apg/.trans/` for plans; also the name of
    /// the layer's `.trans` mirror tier dir.
    pub const fn layer_dir(self) -> &'static str {
        match self {
            Layer::Requirements => "requirements",
            Layer::Domain => "domain",
            Layer::Solution => "solution",
            Layer::Plans => "plans",
            Layer::Implementation => "implementation",
            Layer::Global => "global",
        }
    }

    /// The node types the layer may hold (SPEC §3.1 table): lowercase type
    /// strings — the per-type dir names under the layer and the node-file
    /// schema's `type` field. Plans: `plan-phase` and `task`, plus the four
    /// code kinds (`module`/`file`/`struct`/`function`) a **planned
    /// Implementation node** can declare (schema.rs `planned_node`; they live
    /// as records under `.trans/plans/`, never as node files).
    /// Implementation: only the attach-only `note`/`constraint` files — the
    /// layer's real nodes are the scanned code.
    pub const fn node_types(self) -> &'static [&'static str] {
        match self {
            Layer::Requirements => &["stakeholder", "user", "requirement", "note", "constraint"],
            Layer::Domain => &["group", "entity", "value", "service", "note", "constraint"],
            Layer::Solution => &[
                "system",
                "container",
                "component",
                "person",
                "note",
                "constraint",
            ],
            Layer::Plans => &["plan-phase", "task", "module", "file", "struct", "function"],
            Layer::Implementation => &["note", "constraint"],
            Layer::Global => &["constraint", "note"],
        }
    }

    /// Where the layer's authored nodes serialize (SPEC §3.1): the four
    /// file-backed layers are [`StoragePolicy::Durable`]; plans is
    /// [`StoragePolicy::TransientPlans`] (`.trans/plans/` only, per branch);
    /// implementation is [`StoragePolicy::ScannedCode`] — scanned code, never
    /// serialized, only its attach-only types have durable files.
    pub const fn storage(self) -> StoragePolicy {
        match self {
            Layer::Requirements | Layer::Domain | Layer::Solution | Layer::Global => {
                StoragePolicy::Durable
            }
            Layer::Plans => StoragePolicy::TransientPlans,
            Layer::Implementation => StoragePolicy::ScannedCode,
        }
    }
}

/// The durable `apg/layers/` tree (SPEC §4.1): the file-backed layer dirs in
/// layout order — requirements, domain, solution, implementation, global
/// (plans has no durable dir) — each with its per-type node dirs.
// (Unused until the phase-3 write/ingest tasks join this onto a layout root.)
#[allow(dead_code)]
pub const LAYERS_TREE: &[(&str, &[&str])] = &[
    (
        "requirements",
        &["stakeholder", "user", "requirement", "note", "constraint"],
    ),
    (
        "domain",
        &["group", "entity", "value", "service", "note", "constraint"],
    ),
    (
        "solution",
        &[
            "system",
            "container",
            "component",
            "person",
            "note",
            "constraint",
        ],
    ),
    ("implementation", &["note", "constraint"]),
    ("global", &["constraint", "note"]),
];

/// The `.trans` tree (SPEC §4.1/§5): every layer dir under the transient
/// root — `plans/` first (the per-branch plan store; also the tier dir of
/// plan nodes), then the five feedback mirrors in the tier dir of the
/// attached node. All six tiers, incl. implementation and global.
// (Unused until the phase-3/4 write/ingest tasks join this onto a layout root.)
#[allow(dead_code)]
pub const TRANS_MIRRORS: [Layer; 6] = [
    Layer::Plans,
    Layer::Requirements,
    Layer::Domain,
    Layer::Solution,
    Layer::Implementation,
    Layer::Global,
];
