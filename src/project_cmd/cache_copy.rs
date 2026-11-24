//! `apg project start`'s toolchain-cache materialisation
//! (`requirements.requirement.cache-copy-policy-falls-back-to-a-copy`,
//! `requirements.requirement.project-start-clones-the-toolchain-cache`,
//! `domain.value.cache-copy-policy`).
//!
//! The new worktree's toolchain cache (`<worktree>/apg/.trans/cache`) is seeded
//! from the main checkout's (`<main>/apg/.trans/cache`) by the policy resolved
//! from the command-line flag over the environment over `apg/config.json` over
//! `auto` ([`resolve_policy`]). [`materialise`] builds the whole tree into a
//! fresh temp sibling and renames it into place, so a failure leaves no partial
//! cache behind and surfaces the destination path and the OS error.
//!
//! The module is deliberately pure policy plus real I/O: the caller reads the
//! config value and the environment and passes the raw strings in, so the
//! resolver never loads [`crate::classify::ApgConfig`] and the unit tests drive
//! it with plain `Option<&str>`s.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::Context;

/// The project-start cache-copy choice (`domain.value.cache-copy-policy`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheCopyPolicy {
    /// Clone when the filesystem supports a CoW clone, else fall back to a copy.
    Auto,
    /// Clone only; an unsupported filesystem fails with a named error.
    Clone,
    /// Always a full recursive copy.
    Copy,
    /// Create no cache subtree at all.
    Skip,
}

impl CacheCopyPolicy {
    /// Parse one policy token: exactly `auto`, `clone`, `copy` or `skip`
    /// (surrounding whitespace ignored). Anything else is a named error
    /// listing the accepted values.
    pub fn parse(value: &str) -> anyhow::Result<Self> {
        match value.trim() {
            "auto" => Ok(Self::Auto),
            "clone" => Ok(Self::Clone),
            "copy" => Ok(Self::Copy),
            "skip" => Ok(Self::Skip),
            other => anyhow::bail!(
                "invalid cache-copy policy `{other}` — accepted values: auto, clone, copy, skip"
            ),
        }
    }

    /// The policy's canonical token, for error messages and documentation.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Clone => "clone",
            Self::Copy => "copy",
            Self::Skip => "skip",
        }
    }
}

/// Resolve the policy from the already-read candidate values: the first present
/// one wins, and all absent resolves to [`CacheCopyPolicy::Auto`]. Each present
/// value is parsed by [`CacheCopyPolicy::parse`]; an invalid one names its
/// source (CLI, environment or config) and the value.
///
/// Pure by construction — the caller passes the values in, so this performs no
/// config load, no environment read and no I/O.
pub fn resolve_policy(
    cli: Option<&str>,
    env: Option<&str>,
    config: Option<&str>,
) -> anyhow::Result<CacheCopyPolicy> {
    if let Some(value) = cli {
        return parse_named("--cache-copy", value);
    }
    if let Some(value) = env {
        return parse_named("APG_CACHE_COPY", value);
    }
    if let Some(value) = config {
        return parse_named("apg/config.json cache.project-start", value);
    }
    Ok(CacheCopyPolicy::Auto)
}

fn parse_named(source: &str, value: &str) -> anyhow::Result<CacheCopyPolicy> {
    CacheCopyPolicy::parse(value).with_context(|| format!("invalid {source} value {value:?}"))
}

/// The CoW capability-error set: an errno in this set means a clone failed
/// because the filesystem cannot clone, so [`CacheCopyPolicy::Auto`] falls back
/// to a copy. Compared with `||` rather than `match` because `ENOTSUP` and
/// `EOPNOTSUPP` alias on macOS/Linux (unreachable match arms otherwise).
pub fn is_cow_unsupported(errno: i32) -> bool {
    errno == libc::ENOTSUP
        || errno == libc::EOPNOTSUPP
        || errno == libc::EXDEV
        || errno == libc::ENOTTY
        || errno == libc::EINVAL
}

/// Materialise `src` at `dst` by `policy`.
///
/// A no-op for [`CacheCopyPolicy::Skip`], a missing (or non-directory) `src`,
/// or an existing `dst`. Otherwise the tree is built into a fresh temp sibling
/// under `dst`'s parent and renamed into place, so a failure leaves no partial
/// cache: any error names `dst` and the OS error.
pub fn materialise(src: &Path, dst: &Path, policy: CacheCopyPolicy) -> anyhow::Result<()> {
    if policy == CacheCopyPolicy::Skip || !src.is_dir() || dst.exists() {
        return Ok(());
    }
    let tmp = temp_sibling(dst)?;
    let parent = dst.parent().unwrap_or_else(|| Path::new(""));
    std::fs::create_dir_all(parent).map_err(|e| materialise_error(dst, &e))?;
    // Drop any stale sibling a crashed earlier run left behind (pid-keyed, so
    // only ever ours); never reuse it.
    if tmp.exists() {
        let _ = std::fs::remove_dir_all(&tmp);
        let _ = std::fs::remove_file(&tmp);
    }

    let built = match policy {
        CacheCopyPolicy::Copy => copy_tree(src, &tmp),
        CacheCopyPolicy::Clone => cow_clone(src, &tmp),
        CacheCopyPolicy::Auto => match cow_clone(src, &tmp) {
            Ok(()) => Ok(()),
            Err(e) if e.raw_os_error().is_some_and(is_cow_unsupported) => {
                // The partial clone (Linux builds file-by-file) is discarded
                // and the full copy starts from an empty temp.
                let _ = std::fs::remove_dir_all(&tmp);
                copy_tree(src, &tmp)
            }
            Err(e) => Err(e),
        },
        CacheCopyPolicy::Skip => unreachable!("Skip returned above"),
    };
    if let Err(e) = built {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(materialise_error(dst, &e));
    }
    std::fs::rename(&tmp, dst).map_err(|e| {
        let _ = std::fs::remove_dir_all(&tmp);
        materialise_error(dst, &e)
    })?;
    Ok(())
}

/// `.<name>.tmp-<pid>` beside `dst`, the same parent so the final rename stays
/// on one filesystem and is atomic.
fn temp_sibling(dst: &Path) -> anyhow::Result<PathBuf> {
    let name = dst
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("cache destination {} has no file name", dst.display()))?;
    let mut tmp = OsString::from(".");
    tmp.push(name);
    tmp.push(format!(".tmp-{}", std::process::id()));
    Ok(dst.parent().unwrap_or_else(|| Path::new("")).join(tmp))
}

fn materialise_error(dst: &Path, e: &io::Error) -> anyhow::Error {
    anyhow::anyhow!(
        "failed to materialise the toolchain cache at {}: {e}",
        dst.display()
    )
}

/// The full recursive copy fallback: directories, regular files, and symlinks
/// recreated as symlinks (never followed, so a link out of the cache stays a
/// link).
fn copy_tree(src: &Path, dst: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            copy_tree(&from, &to)?;
        } else if file_type.is_symlink() {
            std::os::unix::fs::symlink(std::fs::read_link(&from)?, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// A copy-on-write clone of the whole tree; `dst` must not exist.
#[cfg(target_os = "macos")]
fn cow_clone(src: &Path, dst: &Path) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt;

    let src = std::ffi::CString::new(src.as_os_str().as_bytes())
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    let dst = std::ffi::CString::new(dst.as_os_str().as_bytes())
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    // `clonefile` clones a directory recursively.
    let rc = unsafe { libc::clonefile(src.as_ptr(), dst.as_ptr(), 0) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// A copy-on-write clone of the whole tree; `dst` must not exist.
#[cfg(target_os = "linux")]
fn cow_clone(src: &Path, dst: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            cow_clone(&from, &to)?;
        } else if file_type.is_symlink() {
            std::os::unix::fs::symlink(std::fs::read_link(&from)?, &to)?;
        } else {
            clone_file(&from, &to)?;
        }
    }
    Ok(())
}

/// `ioctl(dst, FICLONE, src)` for one file, then the source mode.
#[cfg(target_os = "linux")]
fn clone_file(src: &Path, dst: &Path) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::io::AsRawFd;

    let from = std::fs::File::open(src)?;
    let to = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(dst)?;
    let rc = unsafe { libc::ioctl(to.as_raw_fd(), libc::FICLONE, from.as_raw_fd()) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    std::fs::set_permissions(dst, from.metadata()?.permissions())?;
    Ok(())
}

/// No CoW clone on other platforms: report `ENOTSUP` so `auto` falls back and
/// `clone` fails with the named error.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn cow_clone(_src: &Path, _dst: &Path) -> io::Result<()> {
    Err(io::Error::from_raw_os_error(libc::ENOTSUP))
}
