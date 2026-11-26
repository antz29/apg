//! Module mapper: groups the claimed Terraform files of a checkout into
//! configuration modules, one per directory. Pure over paths: the caller passes
//! the sorted `source::claimed_files` list and the scan root.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// One configuration module: its files as repo-relative `/`-separated paths.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Module {
    /// Base configuration files (`.tf`, `.tf.json`, `.tofu`, `.tofu.json`).
    pub base: Vec<String>,
    /// Override files, applied after `base` in lexicographic file-name order.
    pub overrides: Vec<String>,
    /// Test files (`.tftest.hcl`, `.tofutest.hcl`) owned by this module.
    pub tests: Vec<String>,
    /// `.tfvars` and `.tfvars.json` files.
    pub tfvars: Vec<String>,
    /// `.terraform.lock.hcl` files.
    pub locks: Vec<String>,
    /// Same-directory files shadowed by a `.tofu`-family file: File node only.
    pub shadowed: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Base,
    Override,
    Test,
    Tfvars,
    Lock,
}

/// Claimed suffix, kind, and whether it is the `.tofu` family (shadowing).
const SUFFIXES: [(&str, Kind, bool); 9] = [
    (".terraform.lock.hcl", Kind::Lock, false),
    (".tftest.hcl", Kind::Test, false),
    (".tofutest.hcl", Kind::Test, true),
    (".tfvars.json", Kind::Tfvars, false),
    (".tfvars", Kind::Tfvars, false),
    (".tf.json", Kind::Base, false),
    (".tofu.json", Kind::Base, true),
    (".tf", Kind::Base, false),
    (".tofu", Kind::Base, true),
];

struct Claim {
    path: String,
    owner: String,
    dir: String,
    stem: String,
    kind: Kind,
    tofu: bool,
}

/// Repo-relative `/`-separated path of `path` under `root`; the root is "".
pub fn rel(root: &Path, path: &Path) -> String {
    let rest = path.strip_prefix(root).unwrap_or(path);
    rest.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn parent_of(p: &str) -> &str {
    p.rsplit_once('/').map_or("", |(d, _)| d)
}

fn leaf(p: &str) -> &str {
    p.rsplit_once('/').map_or(p, |(_, l)| l)
}

/// Groups `files` (under `root`) into modules keyed by identity: the repo-relative
/// directory, "" for the root. A directory's module is `modules[&rel(root, dir)]`.
/// Names match case-insensitively, as the source claim does.
pub fn modules(root: &Path, files: &[PathBuf]) -> BTreeMap<String, Module> {
    let claims: Vec<Claim> = files
        .iter()
        .filter_map(|file| {
            let path = rel(root, file);
            let dir = parent_of(&path).to_string();
            let name = leaf(&path).to_ascii_lowercase();
            let &(suffix, kind, tofu) = SUFFIXES.iter().find(|(s, _, _)| name.ends_with(*s))?;
            let stem = name[..name.len() - suffix.len()].to_string();
            let kind = if kind == Kind::Base && (stem == "override" || stem.ends_with("_override"))
            {
                Kind::Override
            } else {
                kind
            };
            let owner = if kind == Kind::Test && leaf(&dir).eq_ignore_ascii_case("tests") {
                parent_of(&dir).to_string()
            } else {
                dir.clone()
            };
            Some(Claim {
                path,
                owner,
                dir,
                stem,
                kind,
                tofu,
            })
        })
        .collect();

    let tofu: BTreeSet<(&str, &str, bool)> = claims
        .iter()
        .filter(|c| c.tofu)
        .map(|c| (c.dir.as_str(), c.stem.as_str(), c.kind == Kind::Test))
        .collect();

    let mut out: BTreeMap<String, Module> = BTreeMap::new();
    for c in &claims {
        let shadowed = !c.tofu
            && matches!(c.kind, Kind::Base | Kind::Override | Kind::Test)
            && tofu.contains(&(c.dir.as_str(), c.stem.as_str(), c.kind == Kind::Test));
        let m = out.entry(c.owner.clone()).or_default();
        let list = if shadowed {
            &mut m.shadowed
        } else {
            match c.kind {
                Kind::Base => &mut m.base,
                Kind::Override => &mut m.overrides,
                Kind::Test => &mut m.tests,
                Kind::Tfvars => &mut m.tfvars,
                Kind::Lock => &mut m.locks,
            }
        };
        list.push(c.path.clone());
    }
    for m in out.values_mut() {
        m.base.sort();
        m.overrides.sort();
        m.tests.sort();
        m.tfvars.sort();
        m.locks.sort();
        m.shadowed.sort();
    }
    out
}
