//! The per-worktree toolchain cache layout — the checkout-local directories the
//! Go, Java and TypeScript frontends write their own native build caches into
//! (`requirements.requirement.toolchain-cache-is-per-worktree`,
//! `domain.value.toolchain-cache-path`).
//!
//! The layout is `<checkout>/apg/.trans/cache/<language>/`: the cache root lives
//! under the checkout's own `apg/` root, so every worktree owns a cache and a
//! fact-store format or config change leaves it intact. It is deliberately
//! **not** the shared fact store (`<git-common-dir>/apg`, holding `facts.db`
//! and `facts.lock`): nothing here consults the git common dir, a cache key or
//! the binary version — these are pure path arithmetic over the apg root the
//! caller passes in, so scan's frontend hand-off and `apg project start`'s
//! cache materialisation agree on one layout without touching the filesystem.

use std::path::{Path, PathBuf};

/// The toolchain cache root for the checkout whose `apg/` root is `apg_root`:
/// `<apg_root>/.trans/cache`.
pub fn cache_root(apg_root: &Path) -> PathBuf {
    apg_root.join(".trans").join("cache")
}

/// One language's toolchain cache directory: `<cache_root>/<language>` (Go
/// `go`, Java `java`, TypeScript `ts`).
pub fn language_dir(cache_root: &Path, language: &str) -> PathBuf {
    cache_root.join(language)
}
