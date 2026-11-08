//! One-read ignore probing for the scan's hot path.
//!
//! [`crate::git::state::path_is_ignored`] re-discovers the repository and
//! re-parses the whole git index on every call — fine for a one-off identity
//! probe, ruinous when the ingestor asks the question per record.
//! [`IgnoreProbe`] pays that cost once: it discovers the repo, obtains the
//! index and builds the tracked-path set a single time, then keeps the
//! `git2::Repository` and the `git2::Index` alive. libgit2 caches the index
//! behind a weak pointer, so keeping the Rust `Index` alive is what stops the
//! next lookup re-reading the whole (tens-of-MB) index from disk.

use std::cell::RefCell;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::git::state::canonical_allow_missing;

/// A reusable gitignore oracle: one repository discovery and index read, then
/// cheap verdicts for many paths.
pub struct IgnoreProbe {
    /// `None` when `root` is not inside a git repository: an inert probe that
    /// reports nothing ignored (today's `discover_repo` failure path).
    repo: Option<git2::Repository>,
    /// Held, never read directly: dropping it frees the index libgit2 caches
    /// behind a weak pointer, forcing the next lookup to re-read it.
    _index: Option<git2::Index>,
    /// Workdir-relative paths present at index stage 0 — the "tracked" set the
    /// ignore rules must never override.
    tracked: HashSet<PathBuf>,
}

impl IgnoreProbe {
    /// Discover the enclosing repository and read its index exactly once.
    ///
    /// A missing repository leaves an inert probe (nothing is ignored), and an
    /// unreadable index leaves a rule-only probe — both match the verdict
    /// [`crate::git::state::path_is_ignored`] reaches on the same failures.
    pub fn new(root: &Path) -> Self {
        let Some(repo) = git2::Repository::discover(root).ok() else {
            return Self {
                repo: None,
                _index: None,
                tracked: HashSet::new(),
            };
        };
        let mut tracked = HashSet::new();
        let index = repo.index().ok().inspect(|index| {
            for entry in index.iter() {
                // `path_is_ignored` asks `get_path(rel, 0)`: only a stage-0
                // entry counts as tracked, not a conflict's stages 1–3.
                if entry.flags & 0x3000 == 0 {
                    tracked.insert(PathBuf::from(
                        String::from_utf8_lossy(&entry.path).into_owned(),
                    ));
                }
            }
        });
        Self {
            repo: Some(repo),
            _index: index,
            tracked,
        }
    }

    /// True when the checkout's ignore rules cover `path`.
    ///
    /// `path` accepts any spelling the scan produces: an absolute scanner path
    /// (possibly through a `/var` → `/private/var` symlink) is canonicalized
    /// and made relative to the canonicalized workdir; a relative input is
    /// already workdir-relative. A path outside the checkout is not ignored.
    /// The empty-path and tracked-precedence rules live in [`decide_ignored`].
    pub fn is_ignored(&self, path: &Path) -> bool {
        let Some(repo) = self.repo.as_ref() else {
            return false;
        };
        let Some(workdir) = repo.workdir() else {
            return false;
        };
        let rel = if path.is_absolute() {
            match canonical_allow_missing(path).strip_prefix(canonical_allow_missing(workdir)) {
                Ok(rel) => rel.to_path_buf(),
                Err(_) => return false,
            }
        } else {
            path.to_path_buf()
        };
        decide_ignored(&rel, &self.tracked, |rel| {
            repo.status_should_ignore(rel).unwrap_or(false)
        })
    }
}

/// The pure verdict behind [`IgnoreProbe::is_ignored`]: `rel` is already
/// workdir-relative, `tracked` is the index's stage-0 path set, and
/// `ignored_by_rules` answers the checkout's ignore rules for a relative path.
///
/// An empty path and a tracked path are never ignored — Git's ignore rules
/// apply to untracked content only, and treating a tracked file that happens to
/// match an ignore glob as ignored would silently drop it from the graph.
/// Everything else is whatever the oracle says. Keeping these rules free of
/// git, filesystem and process I/O lets a unit test drive every verdict.
pub fn decide_ignored(
    rel: &Path,
    tracked: &HashSet<PathBuf>,
    ignored_by_rules: impl FnOnce(&Path) -> bool,
) -> bool {
    if rel.as_os_str().is_empty() {
        return false;
    }
    if tracked.contains(rel) {
        return false;
    }
    ignored_by_rules(rel)
}

thread_local! {
    /// The probe installed for the current scan on this thread.
    ///
    /// [`IgnoreProbe`] holds a `git2::Repository`/`git2::Index`, neither of
    /// which is `Sync`, so the context cannot be a plain global — and the scan
    /// path that asks the question is single-threaded by construction.
    static SCAN_PROBE: RefCell<Option<IgnoreProbe>> = const { RefCell::new(None) };
}

/// Installs an [`IgnoreProbe`] as the current thread's scan context for as long
/// as the guard lives, restoring the displaced context on drop.
///
/// RAII rather than paired install/uninstall calls so an early `?` return or a
/// panic still unwinds the previous context.
pub struct ScanProbeGuard {
    /// The context this install displaced, put back by [`Drop`].
    previous: Option<IgnoreProbe>,
}

impl ScanProbeGuard {
    /// Move `probe` into the thread-local context, handing back whatever probe
    /// it replaced.
    pub fn install(probe: IgnoreProbe) -> Self {
        let previous = SCAN_PROBE.with(|slot| slot.borrow_mut().replace(probe));
        Self { previous }
    }
}

impl Drop for ScanProbeGuard {
    fn drop(&mut self) {
        SCAN_PROBE.with(|slot| {
            *slot.borrow_mut() = self.previous.take();
        });
    }
}
