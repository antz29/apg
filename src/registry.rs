//! The project registry: the main checkout's binary-owned map from a project
//! name to the worktree, branch, owning session and creation time of its live
//! context, at `<main>/apg/.trans/projects.db`.
//!
//! The store is local, disposable and outside version control: [`Registry`]
//! creates it on first open, and nothing durable is lost if a stale store is
//! discarded and rebuilt.
//! One worktree belongs to exactly one project name — [`Registry::put`] and
//! [`Registry::replace_all`] refuse a worktree already registered under
//! another name.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition, TableError};

/// The registry database file name inside the main checkout's `apg/.trans/`.
pub const REGISTRY_FILE: &str = "projects.db";

/// The one table: project name -> postcard-encoded [`RegistryEntry`].
const PROJECTS: TableDefinition<&str, &[u8]> = TableDefinition::new("projects");

/// One registered project context: the identity a project name resolves to.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RegistryEntry {
    /// The project name (`apg project start <name>`).
    pub name: String,
    /// The absolute project worktree path.
    pub worktree: PathBuf,
    /// The project branch, when git resolved it.
    pub branch: Option<String>,
    /// The owning session id (`APG_CALLER_SESSION`), or none for `main` and an
    /// ownerless (reconstructed) project.
    pub owner: Option<String>,
    /// Creation time, in seconds since the unix epoch.
    pub created_at: u64,
}

impl RegistryEntry {
    /// A fresh ownerless row for `name` at `worktree`, created now.
    pub fn new(name: impl Into<String>, worktree: impl Into<PathBuf>) -> RegistryEntry {
        RegistryEntry {
            name: name.into(),
            worktree: worktree.into(),
            branch: None,
            owner: None,
            created_at: now_unix(),
        }
    }
}

/// The registry path under a main checkout root:
/// `<main>/apg/.trans/projects.db`.
pub fn registry_path(main_root: &Path) -> PathBuf {
    main_root.join("apg").join(".trans").join(REGISTRY_FILE)
}

/// Opens the project registry of the main checkout at `main_root`, refusing a
/// missing or unreadable store instead of creating one or falling back to any
/// other checkout's rows. The caller supplies the main root, so this read path
/// never consults the cwd.
pub fn require_registry(main_root: &Path) -> Result<Registry> {
    let path = registry_path(main_root);
    let exists = path
        .try_exists()
        .with_context(|| format!("reading the project registry `{}`", path.display()))?;
    if !exists {
        bail!(
            "the project registry `{}` does not exist; rebuild it with `apg project reconstruct`, or run `apg init` for a fresh layout",
            path.display()
        );
    }
    Registry::open(&path).with_context(|| {
        format!(
            "the project registry `{}` is unusable; rebuild it with `apg project reconstruct`, or run `apg init` for a fresh layout",
            path.display()
        )
    })
}

/// Validates one registry row against the repository it names: the built-in
/// `main` row must be the main checkout on the repo's default branch, and any
/// other row must be the live linked worktree of that name on the row's
/// branch. A stale or foreign row is refused — naming the project, its
/// worktree and its branch — so a caller never operates on a checkout the row
/// does not describe. Pure validation: nothing is written.
pub fn validate_row(main_root: &Path, entry: &RegistryEntry) -> Result<()> {
    let apg_root = main_root.join(crate::specs::LAYOUT);
    let identity = crate::git::repo_identity(&apg_root)?;
    let row_branch = entry.branch.as_deref().unwrap_or("<detached>");
    if entry.name == "main" {
        let matches_worktree =
            crate::git::state::canonical_allow_missing(&entry.worktree) == identity.main_root;
        if !matches_worktree || entry.branch != identity.default_branch {
            bail!(
                "refused: project `main` does not match this repository — the registry row names worktree `{}` on branch `{}`, but the main checkout is `{}` on default branch `{}`. Fix: rebuild the registry with `apg project reconstruct`.",
                entry.worktree.display(),
                row_branch,
                identity.main_root.display(),
                identity.default_branch.as_deref().unwrap_or("<detached>")
            );
        }
        return Ok(());
    }

    let repo = git2::Repository::open(&identity.main_root).with_context(|| {
        format!(
            "opening the repository at `{}` for project `{}`",
            identity.main_root.display(),
            entry.name
        )
    })?;
    // The working directory comes from a repository opened on the worktree (a
    // worktree's admin path is not its checkout), canonical and live.
    let linked = repo.find_worktree(&entry.name).ok().and_then(|wt| {
        let wt_repo = git2::Repository::open_from_worktree(&wt).ok()?;
        let worktree = std::fs::canonicalize(wt_repo.workdir()?).ok()?;
        let branch = if wt_repo.head_detached().unwrap_or(true) {
            None
        } else {
            wt_repo
                .head()
                .ok()
                .and_then(|head| head.shorthand().map(str::to_string))
        };
        Some((worktree, branch))
    });
    let Some((worktree, branch)) = linked else {
        bail!(
            "refused: project `{}` is registered at worktree `{}` on branch `{}`, but this repository has no live linked worktree of that name. Fix: rebuild the registry with `apg project reconstruct`.",
            entry.name,
            entry.worktree.display(),
            row_branch
        );
    };
    if worktree != crate::git::state::canonical_allow_missing(&entry.worktree) {
        bail!(
            "refused: project `{}` is registered at worktree `{}` on branch `{}`, but its linked worktree is the checkout at `{}`. Fix: rebuild the registry with `apg project reconstruct`.",
            entry.name,
            entry.worktree.display(),
            row_branch,
            worktree.display()
        );
    }
    if branch != entry.branch {
        bail!(
            "refused: project `{}` is registered at worktree `{}` on branch `{}`, but its linked worktree has branch `{}` checked out. Fix: rebuild the registry with `apg project reconstruct`.",
            entry.name,
            entry.worktree.display(),
            row_branch,
            branch.as_deref().unwrap_or("<detached>")
        );
    }
    Ok(())
}

/// Authorises one registry row for the caller: the built-in `main` row is
/// readable by every session, an ownerless row is refused until claimed, and
/// any other row is authorised only for the owning session or a session in the
/// caller's parentID chain. `current_branch` is the caller's branch, resolved
/// by the caller so this check stays cwd-free; every refusal names the project,
/// the caller's branch and one `apg project claim` fix line. Pure check: no
/// registry write and no git I/O.
pub fn require_owner(
    entry: &RegistryEntry,
    caller: &Caller,
    current_branch: Option<&str>,
) -> Result<()> {
    if entry.name == "main" {
        return Ok(());
    }
    let current = current_branch.unwrap_or("<detached>");
    let Some(owner) = entry.owner.as_deref() else {
        bail!(
            "refused: project `{}` is registered with no owner, so no session may use it until it is claimed; you are on branch `{current}`. Fix: take ownership with `apg project claim {}`.",
            entry.name,
            entry.name
        );
    };
    if caller.session.as_deref() == Some(owner) || caller.ancestors.iter().any(|a| a == owner) {
        return Ok(());
    }
    bail!(
        "refused: project `{}` is owned by session `{owner}`, and you are on branch `{current}`; only the owning session or a session in its parentID chain may use it. Fix: take ownership with `apg project claim {}`.",
        entry.name,
        entry.name
    )
}

/// Seconds since the unix epoch — the row's creation-time unit.
fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

/// Postcard-encodes one row, naming the row on failure.
fn encode(entry: &RegistryEntry) -> Result<Vec<u8>> {
    postcard::to_allocvec(entry)
        .with_context(|| format!("encoding the registry row for `{}`", entry.name))
}

/// Postcard-decodes one row, naming the row on failure.
fn decode(name: &str, bytes: &[u8]) -> Result<RegistryEntry> {
    postcard::from_bytes(bytes).with_context(|| format!("decoding the registry row for `{name}`"))
}

/// The opened project registry. Every mutation is one write transaction, so a
/// caller observes either the previous rows or the new ones.
pub struct Registry {
    db: Database,
    path: PathBuf,
}

impl Registry {
    /// Opens (or creates) the registry at `path`, creating its parent
    /// directory. An existing store is opened in place; a missing one is
    /// created empty.
    pub fn open(path: &Path) -> Result<Registry> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("creating the registry directory `{}`", parent.display())
            })?;
        }
        let db = if path.exists() {
            Database::open(path)
        } else {
            Database::create(path)
        }
        .with_context(|| format!("opening the project registry `{}`", path.display()))?;
        Ok(Registry {
            db,
            path: path.to_path_buf(),
        })
    }

    /// The registry database path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The row registered for `name`, if any.
    pub fn get(&self, name: &str) -> Result<Option<RegistryEntry>> {
        let txn = self
            .db
            .begin_read()
            .with_context(|| format!("reading the project registry `{}`", self.path.display()))?;
        let table = match txn.open_table(PROJECTS) {
            Ok(table) => table,
            Err(TableError::TableDoesNotExist(_)) => return Ok(None),
            Err(err) => {
                return Err(anyhow::Error::from(err)).with_context(|| {
                    format!("opening the project registry `{}`", self.path.display())
                });
            }
        };
        match table.get(name)? {
            Some(row) => Ok(Some(decode(name, row.value())?)),
            None => Ok(None),
        }
    }

    /// Every registered row, in name order.
    pub fn entries(&self) -> Result<Vec<RegistryEntry>> {
        let txn = self
            .db
            .begin_read()
            .with_context(|| format!("reading the project registry `{}`", self.path.display()))?;
        let table = match txn.open_table(PROJECTS) {
            Ok(table) => table,
            Err(TableError::TableDoesNotExist(_)) => return Ok(Vec::new()),
            Err(err) => {
                return Err(anyhow::Error::from(err)).with_context(|| {
                    format!("opening the project registry `{}`", self.path.display())
                });
            }
        };
        let mut entries = Vec::new();
        for row in table.iter()? {
            let (key, value) = row?;
            entries.push(decode(key.value(), value.value())?);
        }
        Ok(entries)
    }

    /// Inserts or replaces the row addressed by `entry.name`, refusing a
    /// worktree already registered under another name.
    pub fn put(&self, entry: &RegistryEntry) -> Result<()> {
        let bytes = encode(entry)?;
        let txn = self
            .db
            .begin_write()
            .with_context(|| format!("writing the project registry `{}`", self.path.display()))?;
        {
            let mut table = txn.open_table(PROJECTS)?;
            let mut conflict = None;
            for row in table.iter()? {
                let (key, value) = row?;
                if key.value() == entry.name.as_str() {
                    continue;
                }
                let other = decode(key.value(), value.value())?;
                if other.worktree == entry.worktree {
                    conflict = Some(other.name);
                    break;
                }
            }
            if let Some(other) = conflict {
                bail!(
                    "worktree `{}` is already registered as project `{other}`; refusing to register it as `{}`",
                    entry.worktree.display(),
                    entry.name
                );
            }
            table.insert(entry.name.as_str(), bytes.as_slice())?;
        }
        txn.commit().context("committing the project registry")?;
        Ok(())
    }

    /// Removes the row registered for `name`; an absent row is a no-op.
    pub fn remove(&self, name: &str) -> Result<()> {
        let txn = self
            .db
            .begin_write()
            .with_context(|| format!("writing the project registry `{}`", self.path.display()))?;
        {
            let mut table = txn.open_table(PROJECTS)?;
            table.remove(name)?;
        }
        txn.commit().context("committing the project registry")?;
        Ok(())
    }

    /// Replaces every row with `entries` in one transaction, refusing a
    /// duplicate name or a worktree registered by more than one project.
    pub fn replace_all(&self, entries: &[RegistryEntry]) -> Result<()> {
        let mut names = std::collections::BTreeSet::new();
        let mut worktrees = std::collections::BTreeSet::new();
        for entry in entries {
            if !names.insert(entry.name.as_str()) {
                bail!(
                    "the project registry cannot hold two rows named `{}`",
                    entry.name
                );
            }
            if !worktrees.insert(entry.worktree.as_path()) {
                bail!(
                    "worktree `{}` is registered by more than one project",
                    entry.worktree.display()
                );
            }
        }
        let txn = self
            .db
            .begin_write()
            .with_context(|| format!("writing the project registry `{}`", self.path.display()))?;
        {
            let mut table = txn.open_table(PROJECTS)?;
            let mut keys = Vec::new();
            for row in table.iter()? {
                let (key, _) = row?;
                keys.push(key.value().to_string());
            }
            for key in keys {
                table.remove(key.as_str())?;
            }
            for entry in entries {
                table.insert(entry.name.as_str(), encode(entry)?.as_slice())?;
            }
        }
        txn.commit().context("committing the project registry")?;
        Ok(())
    }
}

/// The caller's project context, parsed from the environment: `APG_PROJECT`
/// names the registered project, `APG_CALLER_SESSION` the calling session, and
/// `APG_CALLER_ANCESTORS` the comma-separated parentID chain, outermost first.
#[derive(Debug, Clone)]
pub struct Caller {
    /// The project name (`APG_PROJECT`).
    pub project: String,
    /// The calling session id (`APG_CALLER_SESSION`), when set.
    pub session: Option<String>,
    /// The parentID chain (`APG_CALLER_ANCESTORS`), outermost to innermost.
    pub ancestors: Vec<String>,
}

/// Parses the caller's project context from `APG_PROJECT`, `APG_CALLER_SESSION`
/// and the comma-separated `APG_CALLER_ANCESTORS` chain. Returns `None` when
/// `APG_PROJECT` is absent or blank, so a raw CLI call keeps its current
/// behaviour (no project binding).
pub fn caller_from_env() -> Option<Caller> {
    let project = std::env::var("APG_PROJECT").ok()?.trim().to_string();
    if project.is_empty() {
        return None;
    }
    let session = std::env::var("APG_CALLER_SESSION")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let ancestors = std::env::var("APG_CALLER_ANCESTORS")
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|entry| !entry.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    Some(Caller {
        project,
        session,
        ancestors,
    })
}
