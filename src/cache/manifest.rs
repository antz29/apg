//! The content-addressed manifest and the global cache key — split out of the
//! former single-file `src/cache.rs` (phase-04 task-19 wave F). Behaviour
//! preserving: `crate::cache::<name>` keeps resolving through the re-exports in
//! [`crate::cache`].

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::classify::ApgConfig;

/// The scanner JSONL schema/format version folded into [`CacheKey`]. Bump when
/// the wire format changes in a way that invalidates stored facts.
pub const JSONL_SCHEMA_VERSION: &str = "1";

/// The ingestor projection-rule version folded into [`CacheKey`]. Bump when the
/// ingestor's FQN rendering / node projection changes so cached units produced
/// by an older projection are discarded.
///
/// `2` (feedback-102): the cache additionally carries each language's global
/// **module scaffolding** ([`crate::cache::ModuleScaffolding`]) — the
/// pure-intermediate `Module` nodes and every `Module -> Module` `contains` edge
/// that no per-file fact unit can express. A store written by the `1` projection
/// has no scaffolding recorded, so an incremental scan that skips a language
/// would assemble (and export) a graph missing that scaffolding. Bumping the
/// projection version changes the cache-key token, every `1` unit and its
/// missing scaffolding miss, and the drift forces one correctness full scan
/// that records the scaffolding before any reuse can happen.
pub const PROJECTION_RULES_VERSION: &str = "2";

/// The scan config identity folded into the global cache key: the exact set of
/// languages, path excludes, and module restrictions a scan ran with, plus the
/// **classification-config identity** of the loaded `apg/config.json`. A change
/// to any of them invalidates the whole cache (the projection or the per-record
/// `code_type` column could differ).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanConfigKey {
    pub languages: Vec<String>,
    pub excludes: Vec<String>,
    pub modules: Vec<String>,
    /// The digest of the graph-affecting `apg/config.json` fields (the
    /// classification `default` + the ordered `types` rules + the structural
    /// scope), or the distinct sentinel when no config is present — see
    /// [`classification_digest`]. The binary-managed `version` is excluded.
    pub classification: String,
}

/// The classification-config identity digest folded into the cache key: a
/// stable digest of the graph-affecting fields of a loaded `apg/config.json`,
/// or a distinct sentinel when no config is present. Equal configs always
/// digest equal (deterministic across processes); only a genuine change to the
/// classification `default`, the ordered `types` rules (globs/names), or the
/// structural `include`/`exclude`/`code_type` scope moves it. No mtime, no raw
/// whitespace, no binary-managed `version`.
pub fn classification_digest(config: Option<&ApgConfig>) -> String {
    let rendering = match config {
        Some(cfg) => classification_render(cfg),
        None => "\u{0}none".to_string(),
    };
    digest_str(&rendering)
}

/// The canonical rendering of [`ApgConfig`]'s graph-affecting fields: the
/// `default` code type, the `types` rules in **declared order** (first-match
/// wins), and the structural scope. Values are `\0`-separated so a value can
/// never alias a field boundary. The rule order is preserved because it is
/// significant; the inner list order is preserved too for a byte-stable
/// rendering, though it does not change classification.
fn classification_render(cfg: &ApgConfig) -> String {
    let mut out = String::from("default\0");
    out.push_str(&cfg.default);
    for rule in &cfg.types {
        out.push_str("\0type\0");
        out.push_str(&rule.name);
        out.push_str("\0globs\0");
        out.push_str(&rule.globs.join("\0"));
        out.push_str("\0names\0");
        out.push_str(&rule.names.join("\0"));
    }
    out.push_str("\0structural\0");
    match &cfg.structural {
        Some(scope) => {
            out.push_str("exclude\0");
            out.push_str(&scope.exclude.join("\0"));
            out.push_str("\0include\0");
            out.push_str(&scope.include.join("\0"));
            out.push_str("\0code_type\0");
            out.push_str(scope.code_type.as_deref().unwrap_or(""));
        }
        None => out.push_str("none"),
    }
    out
}

/// The version/format/config identity that invalidates the whole cache when it
/// drifts (`domain.value.cache-key`): binary version + JSONL schema/format +
/// ingestor projection rules + the scan config (languages/excludes/modules) +
/// the classification config. A mismatch forces a full scan for correctness,
/// never as a heuristic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheKey {
    pub binary_version: String,
    pub jsonl_schema: String,
    pub projection: String,
    pub config: String,
}

impl CacheKey {
    /// Builds the key for a scan config: the binary's own version plus the
    /// pinned schema/projection versions plus a stable digest of the scan
    /// config (a sorted, `\0`-joined rendering so ordering never matters). The
    /// classification-config identity is already canonical, so it folds in as
    /// one opaque part.
    pub fn compute(config: &ScanConfigKey) -> CacheKey {
        let mut parts: Vec<String> = Vec::new();
        for (tag, vals) in [
            ("lang", &config.languages),
            ("exclude", &config.excludes),
            ("module", &config.modules),
        ] {
            let mut sorted = vals.clone();
            sorted.sort();
            for v in sorted {
                parts.push(format!("{tag}={v}"));
            }
        }
        parts.push(format!("classify={}", config.classification));
        CacheKey {
            binary_version: env!("CARGO_PKG_VERSION").to_string(),
            jsonl_schema: JSONL_SCHEMA_VERSION.to_string(),
            projection: PROJECTION_RULES_VERSION.to_string(),
            config: digest_str(&parts.join("\0")),
        }
    }

    /// The key rendered as one filesystem-safe, collision-resistant token used
    /// as the per-key directory name in the store.
    pub fn token(&self) -> String {
        digest_str(&format!(
            "{}|{}|{}|{}",
            self.binary_version, self.jsonl_schema, self.projection, self.config
        ))
    }

    /// True when this key is compatible with `other` (identical in every
    /// component) — the cache-key drift predicate the fallback uses.
    pub fn matches(&self, other: &CacheKey) -> bool {
        self == other
    }
}

/// A `path -> git blob OID` manifest of a checkout's working tree. Paths are
/// **checkout-relative** with `/` separators so the manifest is portable across
/// worktrees of the same repository (cross-worktree fact sharing).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// The (canonical) absolute scan root the manifest was built from — the
    /// base a reading worktree re-bases stored relative paths onto.
    #[serde(default)]
    pub root: String,
    pub entries: BTreeMap<String, String>,
}

/// The three-way delta between two manifests: paths present only in the newer
/// manifest, paths whose OID changed, and paths that disappeared.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ManifestDelta {
    pub added: BTreeSet<String>,
    pub modified: BTreeSet<String>,
    pub removed: BTreeSet<String>,
}

impl ManifestDelta {
    /// True when nothing changed between the two manifests.
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.modified.is_empty() && self.removed.is_empty()
    }

    /// Every changed path (added ∪ modified ∪ removed).
    pub fn changed(&self) -> BTreeSet<String> {
        self.added
            .iter()
            .chain(&self.modified)
            .chain(&self.removed)
            .cloned()
            .collect()
    }
}

/// The git blob OID of a byte slice — git's own content hash
/// (`Oid::hash_object(ObjectType::Blob, …)`), so a manifest OID is exactly the
/// OID git would assign the same bytes.
pub fn blob_oid_of_bytes(bytes: &[u8]) -> String {
    git2::Oid::hash_object(git2::ObjectType::Blob, bytes)
        .map(|o| o.to_string())
        .unwrap_or_default()
}

/// The git blob OID of a file's current bytes, or `None` when it cannot be
/// read (missing, a directory, an unreadable symlink target).
pub fn blob_oid_of_file(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    Some(blob_oid_of_bytes(&bytes))
}

/// Directories never walked when building a manifest: git's own store, linked
/// worktrees, and the build/dependency trees the scanner excludes. The
/// gitignore check additionally drops `apg/.trans/` and any other ignored
/// content.
const SKIP_DIRS: &[&str] = &[".git", ".worktrees", "target", "node_modules"];

impl Manifest {
    /// Builds the manifest for the scan root `root` (the scanned directory, an
    /// absolute path) by walking its tree and hashing every non-ignored file's
    /// bytes. The root itself is recorded (`root`) so a reading worktree can
    /// re-base stored paths. Gitignored content (the scan's own `apg/.trans/`
    /// outputs, `.worktrees/`) never enters — the manifest is a content
    /// identity of the *scanned* tree, so a scan's own writes cannot invalidate
    /// it.
    pub fn build(root: &Path) -> Manifest {
        let base = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        let repo = git2::Repository::discover(&base).ok();
        let mut entries = BTreeMap::new();
        walk_files(&base, &base, repo.as_ref(), &mut entries);
        Manifest {
            root: base.to_string_lossy().into_owned(),
            entries,
        }
    }

    /// Persists the manifest as JSON under the shared store root
    /// (`<store-root>/manifest.json`).
    pub fn save(&self, store_root: &Path) -> anyhow::Result<()> {
        std::fs::create_dir_all(store_root)?;
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(store_root.join("manifest.json"), json)?;
        Ok(())
    }

    /// Loads a manifest previously saved under the shared store root, or
    /// `None` when none exists / it is unreadable.
    pub fn load(store_root: &Path) -> Option<Manifest> {
        let text = std::fs::read_to_string(store_root.join("manifest.json")).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// The OID recorded for `rel`, if any.
    pub fn oid(&self, rel: &str) -> Option<&str> {
        self.entries.get(rel).map(String::as_str)
    }

    /// Re-bases a relative path recorded under this manifest's root onto
    /// `reader_root` (`/`-joined).
    pub fn rebase_root(&self, reader_root: &str) -> String {
        reader_root.to_string()
    }

    /// The three-way delta from `self` (older) to `other` (newer).
    pub fn diff(&self, other: &Manifest) -> ManifestDelta {
        let mut delta = ManifestDelta::default();
        for (rel, oid) in &other.entries {
            match self.entries.get(rel) {
                None => {
                    delta.added.insert(rel.clone());
                }
                Some(old) if old != oid => {
                    delta.modified.insert(rel.clone());
                }
                Some(_) => {}
            }
        }
        for rel in self.entries.keys() {
            if !other.entries.contains_key(rel) {
                delta.removed.insert(rel.clone());
            }
        }
        delta
    }
}

/// Recursively hashes every non-ignored regular file under `dir` into `out`,
/// keyed by its `/`-separated path relative to `base`.
pub fn walk_files(
    base: &Path,
    dir: &Path,
    repo: Option<&git2::Repository>,
    out: &mut BTreeMap<String, String>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if p.is_dir() {
            if SKIP_DIRS.contains(&name) {
                continue;
            }
            // Ignored directories (`apg/.trans/`, `.worktrees/` when ignored)
            // never enter the manifest.
            if repo.is_some_and(|r| r.status_should_ignore(&p).unwrap_or(false)) {
                continue;
            }
            walk_files(base, &p, repo, out);
        } else {
            // `.git` is the metadata DIR in a primary checkout (already pruned
            // by `SKIP_DIRS`) but a FILE in a linked worktree; skip exactly that
            // entry. A tracked `.gitignore`/`.gitattributes`/`.gitmodules` is
            // ordinary scanned content — the structural `misc` stream graphs it
            // — so it MUST stay in the manifest, or the cached/incremental/splice
            // assemblies drop its File fact and the `misc.` scaffolding.
            if name == ".git" {
                continue;
            }
            if repo.is_some_and(|r| r.status_should_ignore(&p).unwrap_or(false)) {
                continue;
            }
            if let Some(oid) = blob_oid_of_file(&p)
                && let Ok(rel) = p.strip_prefix(base)
            {
                out.insert(rel.to_string_lossy().replace('\\', "/"), oid);
            }
        }
    }
}

/// A stable, dependency-free 128-bit FNV-1a hex digest of a string. Equal bytes
/// always give equal digests across processes; it is only ever compared within
/// the same binary's rule.
pub fn digest_str(s: &str) -> String {
    let mut h1: u64 = 0xcbf2_9ce4_8422_2325;
    let mut h2: u64 = 0x9e37_79b9_7f4a_7c15;
    for (i, b) in s.bytes().enumerate() {
        h1 ^= b as u64;
        h1 = h1.wrapping_mul(0x0000_0100_0000_01b3);
        h2 ^= b as u64 ^ (i as u64);
        h2 = h2.wrapping_mul(0x0000_0100_0000_01b3).rotate_left(5);
    }
    format!("{h1:016x}{h2:016x}")
}
