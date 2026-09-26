//! Crate/module identity rendering and the lib/bin collision prefix map.

use std::collections::HashMap;

use hir::{Crate, Module};

use crate::load::Ctx;
use crate::source::path_of;

/// One local crate's module-prefix inputs: its crate-root path, its cargo
/// package name, and the prefix it renders when no override applies.
struct LocalCrate {
    root: String,
    package: String,
    fallback: String,
}

/// The module-prefix override for one loaded project: crate-root path -> module
/// prefix, for the crates [`crate_prefix`] must not render from its fallback.
///
/// Two cases get an override:
///
/// * a package with EXACTLY ONE local crate target — the cargo package name is
///   preferred over the target/display name (`src/rustlib` declares
///   `[package] name = "apg-rustfrontend"` but `[[bin]] name = "rustfrontend"`,
///   so the display name would shadow the package identity) (phase-05 task-10);
/// * a package whose local crates would otherwise COLLIDE on the same fallback
///   prefix — the default lib + bin package, where both targets carry the
///   package name: the bin gets a distinct `<pkg>-bin` prefix so the two crate
///   roots render distinct module FQNs instead of the ingestor panicking on a
///   duplicate.
///
/// A multi-target package with DISTINCT target names gets no override and keeps
/// exactly its current `rust.foo` / `rust.foo_cli` rendering (rust-analyzer
/// normalizes the bin target's display name; the manifest keeps
/// `[[bin]] name = "foo-cli"`)
/// (`requirements.constraint.rust-crate-fqn-stability`).
pub(crate) fn package_prefix_map(ctx: &Ctx<'_>, crates: &[Crate]) -> HashMap<String, String> {
    let locals: Vec<LocalCrate> = crates
        .iter()
        .filter_map(|k| {
            let root = path_of(ctx, k.root_file(ctx.db));
            let package = ctx.package_by_root.get(&root)?.clone();
            Some(LocalCrate {
                root,
                package,
                fallback: fallback_prefix(ctx, *k),
            })
        })
        .collect();

    let mut counts: HashMap<&str, usize> = HashMap::new();
    for l in &locals {
        *counts.entry(l.package.as_str()).or_insert(0) += 1;
    }

    // Local crates grouped by (package, fallback prefix). A group of more than
    // one is a display-name collision: without an override both crate roots
    // render the same module FQN and the ingestor panics on the duplicate.
    let mut groups: HashMap<(&str, &str), Vec<usize>> = HashMap::new();
    for (i, l) in locals.iter().enumerate() {
        groups
            .entry((l.package.as_str(), l.fallback.as_str()))
            .or_default()
            .push(i);
    }

    let mut prefix: HashMap<String, String> = HashMap::new();
    for (i, l) in locals.iter().enumerate() {
        let group = groups
            .get(&(l.package.as_str(), l.fallback.as_str()))
            .expect("every local crate was grouped above");
        if group.len() < 2 {
            // No collision: the single-target package takes the package-name
            // override; a multi-target package keeps the display-name fallback.
            if counts.get(l.package.as_str()).copied() == Some(1) {
                prefix.insert(l.root.clone(), l.package.clone());
            }
            continue;
        }
        // Collision: the keeper keeps its existing prefix and the bin(s) get a
        // distinct suffix. The keeper is the lib target (root file `lib.rs`)
        // when there is one, else the lowest root path — deterministic, and for
        // the default lib + bin package always the lib.
        let keeper = group
            .iter()
            .copied()
            .find(|&j| is_lib_root(&locals[j].root))
            .unwrap_or_else(|| *group.iter().min().expect("group is non-empty"));
        if i == keeper {
            continue;
        }
        // `<pkg>-bin` for the single colliding bin. Cargo admits at most a lib
        // and a bin sharing a name, so an over-full group is theoretical; a
        // further member is disambiguated by its root file stem.
        let others = group.iter().filter(|&&j| j != keeper).count();
        let suffix = if others <= 1 {
            "bin".to_string()
        } else {
            format!("bin-{}", root_stem(&l.root))
        };
        prefix.insert(l.root.clone(), format!("{}-{suffix}", l.package));
    }
    prefix
}

/// The prefix a crate renders when no override applies: its display name, then
/// its root-module name, then the literal `crate`.
pub(crate) fn fallback_prefix(ctx: &Ctx<'_>, krate: Crate) -> String {
    if let Some(display) = krate.display_name(ctx.db) {
        return display.to_string();
    }
    if let Some(name) = krate.root_module(ctx.db).name(ctx.db) {
        return name.as_str().to_string();
    }
    "crate".to_string()
}

/// Whether a crate-root path is a Cargo library target's root (`src/lib.rs`, or
/// a `[lib] path` ending in `lib.rs`).
pub(crate) fn is_lib_root(root: &str) -> bool {
    std::path::Path::new(root)
        .file_name()
        .is_some_and(|n| n == "lib.rs")
}

/// A crate-root file's stem, for disambiguating an over-full collision group.
pub(crate) fn root_stem(root: &str) -> String {
    std::path::Path::new(root)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| root.to_string())
}

pub(crate) fn crate_prefix(ctx: &Ctx<'_>, krate: Crate) -> String {
    // The loaded project's override takes precedence (phase-05 task-10,
    // libbin-fix): the cargo PACKAGE name for a single-target package, and the
    // distinct `<pkg>-bin` prefix for the bin of a colliding lib + bin package.
    // Everything else renders its per-target display-name-then-root-module-name
    // fallback unchanged, so synthetic/no-manifest crates, distinct-name
    // multi-target packages, and foreign crates are unaffected.
    let root = path_of(ctx, krate.root_file(ctx.db));
    if let Some(pkg) = ctx.package_prefix.get(&root) {
        return pkg.clone();
    }
    fallback_prefix(ctx, krate)
}

pub(crate) fn module_fqn_full(ctx: &Ctx<'_>, m: Module) -> String {
    let prefix = crate_prefix(ctx, m.krate(ctx.db));
    module_fqn_with_prefix(ctx, m, &prefix)
}

pub(crate) fn module_fqn_with_prefix(ctx: &Ctx<'_>, m: Module, prefix: &str) -> String {
    let segments: Vec<String> = m
        .path_segments(ctx.db)
        .map(|n| n.as_str().to_string())
        .collect();
    if segments.is_empty() {
        prefix.to_string()
    } else {
        format!("{prefix}.{}", segments.join("."))
    }
}

pub(crate) fn within_module_limit(ctx: &Ctx<'_>, krate: Crate, module_dirs: &[String]) -> bool {
    if module_dirs.is_empty() {
        return true;
    }
    let path = path_of(ctx, krate.root_file(ctx.db));
    module_dirs.iter().any(|d| path.starts_with(d.as_str()))
}
