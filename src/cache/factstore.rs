//! The shared content-addressed fact store and its path/identity helpers —
//! split out of the former single-file `src/cache.rs` (phase-04 task-19 wave
//! F). Behaviour preserving: `crate::cache::<name>` keeps resolving through the
//! re-exports in [`crate::cache`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::cache::{CacheKey, FileFragment, ModuleScaffolding, digest_str};
use crate::graph::NodeKind;
use crate::specs;

/// The subdirectory under the git common dir that hosts the shared store.
pub const STORE_DIR: &str = "apg";
/// The leaf directory name of the shared fact store.
pub const FACTS_DIR: &str = "facts";
/// The file name holding one language's [`ModuleScaffolding`], inside its
/// `<store>/<lang>/<cache-key>/` directory (feedback-102).
pub const SCAFFOLDING_FILE: &str = "scaffolding.json";

/// An index entry mapping a byte-identity to the stored unit that realizes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactIndexEntry {
    /// The unit's file name under `<root>/<lang>/<cache-key>/`.
    pub unit: String,
    /// The resolution-inputs digest the unit was stored under (the unit is
    /// reusable only when the current file's inputs digest equals this).
    pub inputs: String,
    /// The absolute path the unit was written from (for cross-worktree
    /// path re-basing).
    pub root: String,
    /// The absolute source path the unit was written for.
    pub abs_path: String,
}

/// The shared content-addressed fact store. Root:
/// `<git-common-dir>/apg/facts` — shared across every worktree and branch of
/// the repository, so a fresh worktree reuses the main checkout's units.
pub struct FactStore {
    pub root: PathBuf,
    index: BTreeMap<String, FactIndexEntry>,
}

impl FactStore {
    /// Resolves the store for the repository containing `apg_root`: the repo's
    /// **common** git dir (shared by linked worktrees) plus `apg/facts`.
    pub fn resolve(apg_root: &Path) -> anyhow::Result<FactStore> {
        let repo = git2::Repository::discover(apg_root)?;
        let common = common_git_dir(&repo);
        let root = common.join(STORE_DIR).join(FACTS_DIR);
        Ok(FactStore {
            root,
            index: BTreeMap::new(),
        })
    }

    /// The same store rooted at an explicit directory (tests and callers that
    /// already resolved the common dir).
    pub fn at(root: PathBuf) -> FactStore {
        FactStore {
            root,
            index: BTreeMap::new(),
        }
    }

    /// Loads the store's index from disk (or starts empty when absent).
    pub fn load(mut self) -> FactStore {
        let path = self.root.join("index.json");
        if let Ok(text) = std::fs::read_to_string(&path)
            && let Ok(idx) = serde_json::from_str::<BTreeMap<String, FactIndexEntry>>(&text)
        {
            self.index = idx;
        }
        self
    }

    /// Persists the index (call after writes).
    pub fn save_index(&self) -> anyhow::Result<()> {
        std::fs::create_dir_all(&self.root)?;
        let json = serde_json::to_string_pretty(&self.index)?;
        std::fs::write(self.root.join("index.json"), json)?;
        Ok(())
    }

    /// The per-key directory: `<root>/<lang>/<cache-key-token>/`.
    fn key_dir(&self, lang: &str, cache_key: &CacheKey) -> PathBuf {
        self.root.join(lang).join(cache_key.token())
    }

    /// The index key for a file's byte identity under one cache key.
    fn index_key(lang: &str, rel: &str, oid: &str, cache_key: &CacheKey) -> String {
        format!("{lang}|{rel}|{oid}|{}", cache_key.token())
    }

    /// True when a unit for this byte identity + inputs + cache key exists.
    pub fn has(
        &self,
        lang: &str,
        rel: &str,
        oid: &str,
        inputs: &str,
        cache_key: &CacheKey,
    ) -> bool {
        self.index
            .get(&Self::index_key(lang, rel, oid, cache_key))
            .is_some_and(|e| e.inputs == inputs)
    }

    /// Reads the reusable unit for `(lang, rel, oid)` under `cache_key` —
    /// `Some` only when the stored unit's resolution-inputs digest equals
    /// `inputs`. A byte-identical file whose inputs drifted is NOT reusable.
    pub fn reuse(
        &self,
        lang: &str,
        rel: &str,
        oid: &str,
        inputs: &str,
        cache_key: &CacheKey,
    ) -> Option<(FileFragment, String)> {
        let entry = self
            .index
            .get(&Self::index_key(lang, rel, oid, cache_key))?;
        if entry.inputs != inputs {
            return None;
        }
        let text = std::fs::read_to_string(self.key_dir(lang, cache_key).join(&entry.unit)).ok()?;
        let frag: FileFragment = serde_json::from_str(&text).ok()?;
        Some((frag, entry.root.clone()))
    }

    /// Reads the stored candidate unit for `(lang, rel, oid)` regardless of
    /// input drift, with the writing root — used to discover prior inputs.
    pub fn candidate(
        &self,
        lang: &str,
        rel: &str,
        oid: &str,
        cache_key: &CacheKey,
    ) -> Option<(FileFragment, String)> {
        let entry = self
            .index
            .get(&Self::index_key(lang, rel, oid, cache_key))?;
        let text = std::fs::read_to_string(self.key_dir(lang, cache_key).join(&entry.unit)).ok()?;
        let frag: FileFragment = serde_json::from_str(&text).ok()?;
        Some((frag, entry.root.clone()))
    }

    /// Writes a unit. The unit's file name is a digest over content OID +
    /// resolution-inputs digest + the global cache key, so identical
    /// content+inputs+key always maps to the same stored unit (content
    /// addressing) even across worktrees.
    pub fn put(
        &mut self,
        frag: &FileFragment,
        writer_root: &str,
        cache_key: &CacheKey,
    ) -> anyhow::Result<()> {
        let dir = self.key_dir(&frag.lang, cache_key);
        std::fs::create_dir_all(&dir)?;
        let inputs = frag.inputs_digest();
        let unit = format!(
            "{}.json",
            digest_str(&format!(
                "{}|{}|{}|{}",
                frag.blob_oid,
                inputs,
                cache_key.token(),
                frag.rel_path
            ))
        );
        std::fs::write(dir.join(&unit), serde_json::to_string(frag)?)?;
        let entry = FactIndexEntry {
            unit,
            inputs,
            root: writer_root.to_string(),
            abs_path: abs_path_of(writer_root, &frag.rel_path),
        };
        self.index.insert(
            Self::index_key(&frag.lang, &frag.rel_path, &frag.blob_oid, cache_key),
            entry,
        );
        Ok(())
    }

    /// The number of indexed units (tests).
    pub fn len(&self) -> usize {
        self.index.len()
    }

    /// True when the store holds no units.
    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    /// Writes one language's module scaffolding under its cache-key dir. Stored
    /// beside the per-file units so a cache-key drift discards it with them
    /// (feedback-102).
    pub fn put_scaffolding(
        &self,
        lang: &str,
        scaffolding: &ModuleScaffolding,
        cache_key: &CacheKey,
    ) -> anyhow::Result<()> {
        let dir = self.key_dir(lang, cache_key);
        std::fs::create_dir_all(&dir)?;
        std::fs::write(
            dir.join(SCAFFOLDING_FILE),
            serde_json::to_string(scaffolding)?,
        )?;
        Ok(())
    }

    /// Writes every language's scaffolding from a
    /// [`ModuleScaffolding::extract`] map.
    pub fn put_scaffolding_all(
        &self,
        by_lang: &BTreeMap<String, ModuleScaffolding>,
        cache_key: &CacheKey,
    ) -> anyhow::Result<()> {
        for (lang, scaffolding) in by_lang {
            self.put_scaffolding(lang, scaffolding, cache_key)?;
        }
        Ok(())
    }

    /// Reads a language's module scaffolding, or `None` when none was recorded
    /// under this cache key (a `1`-projection store, or a language never seen).
    pub fn scaffolding(&self, lang: &str, cache_key: &CacheKey) -> Option<ModuleScaffolding> {
        let text =
            std::fs::read_to_string(self.key_dir(lang, cache_key).join(SCAFFOLDING_FILE)).ok()?;
        serde_json::from_str(&text).ok()
    }
}

/// The git **common** dir of a repository — for a linked worktree,
/// `<main>/.git`; for the main checkout, `<checkout>/.git`. Read from the
/// gitdir's `commondir` file when present (the linked-worktree case), else the
/// gitdir itself. Canonicalized when possible; falling back to the lexical
/// path.
pub fn common_git_dir(repo: &git2::Repository) -> PathBuf {
    let gitdir = repo.path();
    let rel = std::fs::read_to_string(gitdir.join("commondir"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    match rel {
        Some(rel) => {
            let joined = gitdir.join(rel);
            std::fs::canonicalize(&joined).unwrap_or(joined)
        }
        None => std::fs::canonicalize(gitdir).unwrap_or_else(|_| gitdir.to_path_buf()),
    }
}

/// The absolute path a relative path names under `root` (`/`-joined).
pub fn abs_path_of(root: &str, rel: &str) -> String {
    if root.ends_with('/') {
        format!("{root}{rel}")
    } else {
        format!("{root}/{rel}")
    }
}

/// The checkout-relative path of an absolute path under `root`, or the path
/// unchanged when it is not under `root`.
pub fn rel_path_of(root: &Path, abs: &str) -> String {
    Path::new(abs)
        .strip_prefix(root)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| abs.to_string())
}

pub fn kind_str(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Module => "module",
        NodeKind::File => "file",
        NodeKind::Struct => "struct",
        NodeKind::Function => "function",
        _ => "unresolved",
    }
}

/// The store root the scan records facts into for `apg_root`, or `None` when
/// `apg_root` is not inside a git repo (no shared store to resolve).
pub fn store_root_for(apg_root: &Path) -> Option<PathBuf> {
    FactStore::resolve(apg_root).ok().map(|s| s.root)
}

/// Convenience: the shared store root under a checkout's `apg/` layout
/// (`<git-common-dir>/apg/facts`).
pub fn store_root_under_apg(apg_root: &Path) -> Option<PathBuf> {
    let _ = specs::TRANS;
    store_root_for(apg_root)
}
