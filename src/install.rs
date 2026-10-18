//! The `apg init` install surface: the embedded opencode suite (tools, lib,
//! upgrade guide, distributed agents), the idempotent install/prune logic, and
//! the `apg/` layout + `.gitignore` scaffolding.
//!
//! Extracted from the crate root as a cohesive single-responsibility module
//! (phase-04 decomposition); no behaviour change — the same embedded bytes
//! (`include_str!` paths stay `../opencode-suite/...`, resolved from `src/`),
//! the same install/update/prune semantics, and the same printed output.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{git, specs, version_gate};

/// The opencode tool suite that `apg init` installs into `~/.config/opencode/`.
/// Each entry is a file under `plugins/` (opencode 2.x plugin discovery under
/// `~/.config/opencode/plugins/`), single-sourced from this repo's
/// `opencode-suite/`. The files shell out to `apg query` / `apg scan` (on PATH)
/// from the project root; see `opencode-suite/lib/apg.ts` for the shared
/// plumbing.
pub const SUITE_TOOLS: &[(&str, &str)] = &[
    (
        "apg_query.ts",
        include_str!("../opencode-suite/tools/apg_query.ts"),
    ),
    (
        "apg_scan.ts",
        include_str!("../opencode-suite/tools/apg_scan.ts"),
    ),
    (
        "apg_find_symbol.ts",
        include_str!("../opencode-suite/tools/apg_find_symbol.ts"),
    ),
    (
        "apg_modules.ts",
        include_str!("../opencode-suite/tools/apg_modules.ts"),
    ),
    (
        "apg_module_files.ts",
        include_str!("../opencode-suite/tools/apg_module_files.ts"),
    ),
    (
        "apg_module_structs.ts",
        include_str!("../opencode-suite/tools/apg_module_structs.ts"),
    ),
    (
        "apg_file_units.ts",
        include_str!("../opencode-suite/tools/apg_file_units.ts"),
    ),
    (
        "apg_file_path.ts",
        include_str!("../opencode-suite/tools/apg_file_path.ts"),
    ),
    (
        "apg_methods.ts",
        include_str!("../opencode-suite/tools/apg_methods.ts"),
    ),
    (
        "apg_struct.ts",
        include_str!("../opencode-suite/tools/apg_struct.ts"),
    ),
    (
        "apg_callers.ts",
        include_str!("../opencode-suite/tools/apg_callers.ts"),
    ),
    (
        "apg_callees.ts",
        include_str!("../opencode-suite/tools/apg_callees.ts"),
    ),
    (
        "apg_uses.ts",
        include_str!("../opencode-suite/tools/apg_uses.ts"),
    ),
    (
        "apg_unresolved.ts",
        include_str!("../opencode-suite/tools/apg_unresolved.ts"),
    ),
    (
        "apg_hunk.ts",
        include_str!("../opencode-suite/tools/apg_hunk.ts"),
    ),
    // Durable node-file mutation surface (apg-projects SPEC §2.2/§4).
    (
        "apg_node.ts",
        include_str!("../opencode-suite/tools/apg_node.ts"),
    ),
    (
        "apg_edge.ts",
        include_str!("../opencode-suite/tools/apg_edge.ts"),
    ),
    // Project lifecycle (start / verify / merge).
    (
        "apg_project.ts",
        include_str!("../opencode-suite/tools/apg_project.ts"),
    ),
    // Live session lifecycle: spawn `apg session start` detached, poll until
    // live, then return while the session keeps running.
    (
        "apg_session.ts",
        include_str!("../opencode-suite/tools/apg_session.ts"),
    ),
    // Plan/review suite.
    (
        "apg_review.ts",
        include_str!("../opencode-suite/tools/apg_review.ts"),
    ),
    (
        "apg_review_add.ts",
        include_str!("../opencode-suite/tools/apg_review_add.ts"),
    ),
    (
        "apg_review_action.ts",
        include_str!("../opencode-suite/tools/apg_review_action.ts"),
    ),
    (
        "apg_review_resolve.ts",
        include_str!("../opencode-suite/tools/apg_review_resolve.ts"),
    ),
    (
        "apg_review_reject.ts",
        include_str!("../opencode-suite/tools/apg_review_reject.ts"),
    ),
    (
        "apg_plan.ts",
        include_str!("../opencode-suite/tools/apg_plan.ts"),
    ),
    (
        "apg_plan_phases.ts",
        include_str!("../opencode-suite/tools/apg_plan_phases.ts"),
    ),
    (
        "apg_plan_tasks.ts",
        include_str!("../opencode-suite/tools/apg_plan_tasks.ts"),
    ),
    (
        "apg_plan_complete.ts",
        include_str!("../opencode-suite/tools/apg_plan_complete.ts"),
    ),
    (
        "apg_plan_render.ts",
        include_str!("../opencode-suite/tools/apg_plan_render.ts"),
    ),
    (
        "apg_plan_add.ts",
        include_str!("../opencode-suite/tools/apg_plan_add.ts"),
    ),
    (
        "apg_plan_done.ts",
        include_str!("../opencode-suite/tools/apg_plan_done.ts"),
    ),
    (
        "apg_plan_undone.ts",
        include_str!("../opencode-suite/tools/apg_plan_undone.ts"),
    ),
    (
        "apg_plan_note.ts",
        include_str!("../opencode-suite/tools/apg_plan_note.ts"),
    ),
    (
        "apg_plan_verify.ts",
        include_str!("../opencode-suite/tools/apg_plan_verify.ts"),
    ),
    // Deterministic durable-spec lint (apg spec lint), the read-only pre-pass
    // the plan-review and spec-review passes run before their semantic review.
    (
        "apg_spec_lint.ts",
        include_str!("../opencode-suite/tools/apg_spec_lint.ts"),
    ),
    // Filesystem scope tools.
    (
        "apg_rm.ts",
        include_str!("../opencode-suite/tools/apg_rm.ts"),
    ),
    (
        "apg_mv.ts",
        include_str!("../opencode-suite/tools/apg_mv.ts"),
    ),
    (
        "apg_cp.ts",
        include_str!("../opencode-suite/tools/apg_cp.ts"),
    ),
];

/// Shared helper module used by the suite plugins (`lib/apg.ts`), installed by
/// `apg init` under `~/.config/opencode/lib/` beside the plugins (outside
/// opencode's plugin discovery).
pub const APG_LIB: &str = include_str!("../opencode-suite/lib/apg.ts");

/// The layout-upgrade guide that `apg init` installs into
/// `~/.config/opencode/lib/apg-upgrade.md` next to the shared lib: version-field
/// meaning, mismatch detection, upgrade steps (re-run init → re-scan),
/// common-issue fixes. The R10 version gate's block text points at it.
///
/// Maintained inline here; if the suite tree (`opencode-suite/lib/`) ever
/// gains an `apg-upgrade.md`, this const should flip to an `include_str!` of
/// it like `APG_LIB` above.
pub const APG_UPGRADE_DOC: &str = r#"# apg layout upgrades — the `apg/config.json` version field

This guide is installed by `apg init` at `~/.config/opencode/lib/apg-upgrade.md`;
the version gate's block text points here.

## What the `version` field is

`apg/config.json` (the committed layout config at the repo root) carries a
**binary-managed `version` field** — for example `"version": "0.10.4"`. It
records which apg binary last initialized or upgraded the layout. It is not a
user setting: `apg init` owns it. Your `default` / `types` (code_type rules)
are yours; the `version` field is the binary's — never hand-edit it.

## What checks it

The layout-touching operations — `apg scan` and `apg project start` —
**block, they never warn**, unless the layout's declared version shares the
running binary's **major.minor** (patch versions never matter):

| apg/config.json `version` | apg 0.10.4 verdict |
|---|---|
| same major.minor (`0.10.0` … `0.10.99`) | proceed (patch diff is fine) |
| missing `version` field (pre-versioning layout) | **block** |
| older major.minor (`0.9.x`, `0.8.x`, …) | **block** — upgrade the layout |
| newer major.minor (`0.11.x`, `1.x`, …) | **block** — upgrade the binary |

`apg init` is the upgrade act: it re-runs idempotently, writes the current
version, and scaffolds the layout. `apg init` is also the layout entry point —
a repo never created by `apg init` has no versioned layout at all.

## Upgrade steps

1. Re-run **`apg init`** from the repo root. It is idempotent: writes the
   current binary version into `apg/config.json` (your code_type rules are
   untouched), scaffolds `apg/.worktrees/` + the `.gitignore` entries
   (`apg/.trans/`, `apg/.worktrees/`), and installs/updates the opencode apg
   suite (tools, agents, this guide).
2. Re-run the blocked command: `apg scan`, or `apg project start <name>`.
3. **Layout newer than the binary** (the block text says so): upgrade apg
   first — `brew upgrade apg` (or the matching frontend formulae), the
   `install.sh` installer, or a newer release — then `apg init`, then re-run
   the blocked command.
4. **The reviewer became a distributed agent.** When upgrading to the release
   that first distributes `implementation-phase-reviewer.md` (0.18.0), a repo
   that already carries its own repo-local
   `.opencode/agents/implementation-phase-reviewer.md` will see `apg init`'s
   duplicate-install warning. That warning is correct — `apg init` never
   deletes anything — so **manually remove** the local
   `.opencode/agents/implementation-phase-reviewer.md` (the installed copy in
   `~/.config/opencode/agents/` now shadows it), then re-run `apg init`.

## Common issues

- **"declares no layout version"** — the layout predates layout versioning,
  or was created without `apg init` (an old `apg scan` created only
  `apg/.trans/`). Fix: `apg init` writes the field.
- **"not a valid version"** — the field was hand-edited into something
  unparseable. Fix: `apg init` rewrites it.
- **"config.json does not exist"** — no versioned layout here yet. Fix:
  `apg init` (the layout entry point), then re-run.
- **Worktree scaffolding missing** — `apg init` scaffolds `apg/.worktrees/`
  and its `.gitignore` entry; `apg project start` also self-heals both when
  absent, so this resolves itself on the next start.
- **`apg init` warns that `.opencode/agents/implementation-phase-reviewer.md`
  duplicates the installed suite** — expected when upgrading to the release
  that first distributes the reviewer (0.18.0): the local file is a leftover
  repo-local copy that now shadows the installed one. `apg init` never deletes
  anything, so **manually remove** the local
  `.opencode/agents/implementation-phase-reviewer.md` and re-run `apg init`.
- **Code_type rules look reformatted** — the JSON file is rewritten when the
  version changes; the rules' *content* is preserved (only whitespace/field
  order may normalize). Never re-add the version by hand afterwards — re-run
  `apg init`.
"#;

/// The `codebase-navigator.md` agent file that `apg init` installs into
/// `~/.config/opencode/`. Auto-discovered by opencode from
/// `~/.config/opencode/agents/*.md`; configured to use the apg suite tools and
/// to guide the user through running `apg scan` on the CLI (there is no in-chat
/// scan tool). Single-sourced from the repo's own agent file.
const CODEBASE_NAVIGATOR_AGENT: &str =
    include_str!("../opencode-suite/agents/codebase-navigator.md");

/// The eight distributed agents that `apg init` installs into
/// `~/.config/opencode/agents/` (SPEC R13/R15): the navigator plus the seven
/// spec/plan/review/builder/migration agents — including the
/// `implementation-phase-reviewer` — single-sourced from the repo's
/// `opencode-suite/agents/`.
pub const AGENTS: &[(&str, &str)] = &[
    ("codebase-navigator.md", CODEBASE_NAVIGATOR_AGENT),
    (
        "spec-writer.md",
        include_str!("../opencode-suite/agents/spec-writer.md"),
    ),
    (
        "plan-writer.md",
        include_str!("../opencode-suite/agents/plan-writer.md"),
    ),
    (
        "spec-review.md",
        include_str!("../opencode-suite/agents/spec-review.md"),
    ),
    (
        "plan-review.md",
        include_str!("../opencode-suite/agents/plan-review.md"),
    ),
    (
        "implementation-phase-reviewer.md",
        include_str!("../opencode-suite/agents/implementation-phase-reviewer.md"),
    ),
    (
        "agent-builder.md",
        include_str!("../opencode-suite/agents/agent-builder.md"),
    ),
    (
        "migration-agent.md",
        include_str!("../opencode-suite/agents/migration-agent.md"),
    ),
];

/// The eight distributed agent filenames — the apg-owned names in
/// `~/.config/opencode/agents/`. `apg init` prunes any of these that a newer release
/// dropped from the suite (a user's own same-named agent is the accepted edge).
const KNOWN_AGENT_FILES: &[&str] = &[
    "codebase-navigator.md",
    "spec-writer.md",
    "plan-writer.md",
    "spec-review.md",
    "plan-review.md",
    "implementation-phase-reviewer.md",
    "agent-builder.md",
    "migration-agent.md",
];

/// The committed changelog at the repo root, embedded so the binary carries the
/// migration convention: a `## vX.Y.Z` header per release, an optional
/// `### Migration` subsection holding the tasks a layout upgrade across that
/// version must perform. `apg init` slices the `(old layout version, binary
/// version]` range into `apg/migration-guide.md` for the migration-agent to work.
pub const CHANGELOG: &str = include_str!("../CHANGELOG.md");

/// The migration-guide filename `apg init` writes into the committed `apg/`
/// layout dir (`apg/migration-guide.md`) and the migration-agent reads.
pub const MIGRATION_GUIDE: &str = "migration-guide.md";

/// The `package.json` written by `apg init` into `~/.config/opencode/` when
/// none exists, so the plugin files' `@opencode/plugin` import resolves.
const OPENCODE_PACKAGE_JSON: &str = r#"{
  "dependencies": {
    "@opencode/plugin": "2.0.29"
  }
}
"#;

const DEFAULT_CONFIG_JSON: &str = r#"{
  "default": "src",
  "types": []
}
"#;

/// The user-level opencode config dir: `~/.config/opencode` — the opencode 2.x
/// global config dir. `apg init` installs the tool suite here (not into the
/// project dir) so it's available to every project's opencode session; tool
/// discovery is project-root based, so the global install works across all
/// projects. `apg init` neither reads nor writes the V1 `~/.opencode/` root.
fn user_opencode_dir() -> anyhow::Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|h| h.join(".config").join("opencode"))
        .ok_or_else(|| anyhow::anyhow!("could not determine home directory (set $HOME)"))
}

/// Writes `content` to `path` only when the file is missing or its contents
/// differ, so an existing install is updated where required. Returns whether a
/// write happened.
fn write_if_changed(path: &Path, content: &str) -> std::io::Result<bool> {
    match std::fs::read(path) {
        Ok(existing) if existing == content.as_bytes() => Ok(false),
        _ => {
            std::fs::write(path, content)?;
            Ok(true)
        }
    }
}

/// Files in a project's local `.opencode/` that duplicate the user-level
/// install (`~/.config/opencode/`): the same relative path exists in both. A
/// project `.opencode/` should hold only project-specific agents (agent-builder
/// generated); a copy of a V2 install location there — `plugins/`, `agents/`,
/// `lib/`, or `package.json` — shadows the installed version, so `apg init`
/// warns loudly about it (the warning is printed by `cmd_init`) — it never
/// deletes anything. Dep-manifest names (`package.json`, `package-lock.json`,
/// `bun.lock`, `.gitignore`) are excluded by `is_dep_manifest`.
pub fn duplicate_install_files(project_opencode: &Path, user_opencode: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, project: &Path, user: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                // Dependency trees are not part of the installed suite.
                if p.file_name().is_some_and(|n| n == "node_modules") {
                    continue;
                }
                walk(&p, project, user, out);
            } else if let Ok(rel) = p.strip_prefix(project)
                && user.join(rel).is_file()
                && !is_dep_manifest(p.file_name())
            {
                out.push(p);
            }
        }
    }
    let mut out = Vec::new();
    if project_opencode.is_dir() {
        walk(project_opencode, project_opencode, user_opencode, &mut out);
    }
    out
}

/// The per-project npm manifests + git hygiene files that `apg init` also
/// writes into `~/.opencode/` (`package.json`, lockfiles, `.gitignore`) are not
/// suite files; a project's own copies are not shadows.
fn is_dep_manifest(name: Option<&std::ffi::OsStr>) -> bool {
    matches!(
        name.and_then(|n| n.to_str()),
        Some("package.json" | "package-lock.json" | "bun.lock" | ".gitignore")
    )
}

/// Prune stale apg-managed files from the user-level install
/// (`~/.config/opencode/`): `plugins/apg_*.ts` (the `apg_` prefix is the apg
/// namespace) and the known distributed agent names that a newer release
/// removed from the suite. The prune is scoped to the `~/.config/opencode/`
/// root alone — it never reads or writes anything under `~/.opencode/`.
/// Anything not owned by apg (a user's own plugins/agents) is preserved.
/// `apg init` already overwrites edited suite files via `write_if_changed`, so
/// removing the same owned set is consistent. Returns the number of files
/// pruned.
pub fn prune_stale_suite(opencode_dir: &Path) -> std::io::Result<usize> {
    let current_tools: std::collections::HashSet<&str> =
        SUITE_TOOLS.iter().map(|(n, _)| *n).collect();
    let current_agents: std::collections::HashSet<&str> = AGENTS.iter().map(|(n, _)| *n).collect();
    let mut pruned = 0;

    let plugins_dir = opencode_dir.join("plugins");
    if let Ok(entries) = std::fs::read_dir(&plugins_dir) {
        for e in entries.flatten() {
            let p = e.path();
            let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if name.starts_with("apg_") && name.ends_with(".ts") && !current_tools.contains(name) {
                std::fs::remove_file(&p)?;
                pruned += 1;
            }
        }
    }

    let agents_dir = opencode_dir.join("agents");
    if let Ok(entries) = std::fs::read_dir(&agents_dir) {
        for e in entries.flatten() {
            let p = e.path();
            let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if KNOWN_AGENT_FILES.contains(&name) && !current_agents.contains(name) {
                std::fs::remove_file(&p)?;
                pruned += 1;
            }
        }
    }

    Ok(pruned)
}

/// Installs (or updates) the apg suite into the V2 `opencode_dir` layout: every
/// `SUITE_TOOLS` plugin file into `plugins/`, the shared lib + upgrade guide
/// into `lib/` (beside the plugins, outside opencode's plugin discovery), the
/// eight distributed agents into `agents/`, and `package.json` at the root —
/// each written only when missing or changed — then prunes stale apg-owned
/// files. Returns `(suite files written/updated, agents written/updated, files
/// pruned)` — the two populations counted separately. `cmd_init` calls it
/// against `~/.config/opencode/`; tests call it against a temp dir. The
/// package.json scaffold is only written when absent (never clobbered). The
/// embedded agent/tool prose it delivers carries the coordinator-mediated
/// feedback cycle: the owning writer returns an ACTIONED/WONT-FIX claim and the
/// coordinator performs the shallow claim-vs-change check and then actions the
/// item (`apg_review_action`).
pub fn install_suite(opencode_dir: &Path) -> anyhow::Result<(usize, usize, usize)> {
    // opencode 2.x discovers its tool plugins in `plugins/`.
    let plugins_dir = opencode_dir.join("plugins");
    std::fs::create_dir_all(&plugins_dir)?;
    let agents_dir = opencode_dir.join("agents");
    std::fs::create_dir_all(&agents_dir)?;

    let pkg_path = opencode_dir.join("package.json");
    if !pkg_path.exists() {
        std::fs::write(&pkg_path, OPENCODE_PACKAGE_JSON)?;
    }
    let mut suite_files = 0usize;
    for (name, content) in SUITE_TOOLS {
        if write_if_changed(&plugins_dir.join(name), content)? {
            suite_files += 1;
        }
    }
    // The shared lib + upgrade guide live in `lib/`, beside the plugins,
    // outside opencode's plugin discovery.
    let lib_dir = opencode_dir.join("lib");
    std::fs::create_dir_all(&lib_dir)?;
    if write_if_changed(&lib_dir.join("apg.ts"), APG_LIB)? {
        suite_files += 1;
    }
    // The upgrade guide the R10 gate's block text points at (task-4).
    // Best-effort with a warning: an unwritable ~/.config/opencode must not
    // fail init (mirrors the npm-install failure handling below).
    let upgrade_doc_path = lib_dir.join("apg-upgrade.md");
    match write_if_changed(&upgrade_doc_path, APG_UPGRADE_DOC) {
        Ok(true) => suite_files += 1,
        Ok(false) => {}
        Err(e) => eprintln!(
            "warning: could not install the upgrade guide into {} ({e}); the version-gate block text points at ~/.config/opencode/lib/apg-upgrade.md — re-run `apg init` once the location is writable",
            upgrade_doc_path.display()
        ),
    }
    let mut agents = 0usize;
    for (name, content) in AGENTS {
        if write_if_changed(&agents_dir.join(name), content)? {
            agents += 1;
        }
    }
    let pruned = prune_stale_suite(opencode_dir)?;
    Ok((suite_files, agents, pruned))
}

/// One `## vX.Y.Z` changelog entry: its parsed version, the version token as
/// written, and the raw content of its optional `### Migration` subsection
/// (empty when the entry has none).
struct ChangelogEntry {
    version: (u64, u64, u64),
    version_text: String,
    migration: String,
}

/// Parses the leading `X.Y[.Z]` of a version into a comparable tuple (a missing
/// patch is `0`). `None` for anything that is not at least two numeric
/// dot-separated components — a malformed changelog header costs its entry, not
/// the whole guide.
fn parse_semver(version: &str) -> Option<(u64, u64, u64)> {
    let mut parts = version.trim().split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next().unwrap_or("0").parse().ok()?;
    Some((major, minor, patch))
}

/// Parses the changelog by convention (pure and infallible): a `## vX.Y.Z` line
/// whose version token parses starts an entry, and a `### Migration` line inside
/// an entry opens its migration content, which runs to the next heading. Prose
/// that merely mentions the heading is not one, and an unparseable version
/// header is skipped rather than fatal.
fn parse_changelog(changelog: &str) -> Vec<ChangelogEntry> {
    let mut entries: Vec<ChangelogEntry> = Vec::new();
    let mut in_migration = false;
    for line in changelog.lines() {
        let trimmed = line.trim();
        if let Some(token) = trimmed
            .strip_prefix("## v")
            .and_then(|rest| rest.split_whitespace().next())
            && let Some(version) = parse_semver(token)
        {
            entries.push(ChangelogEntry {
                version,
                version_text: token.to_string(),
                migration: String::new(),
            });
            in_migration = false;
            continue;
        }
        let Some(entry) = entries.last_mut() else {
            continue;
        };
        if trimmed == "### Migration" {
            in_migration = true;
        } else if trimmed.starts_with('#') {
            in_migration = false;
        } else if in_migration {
            entry.migration.push_str(line);
            entry.migration.push('\n');
        }
    }
    for entry in &mut entries {
        entry.migration = entry.migration.trim().to_string();
    }
    entries
}

/// Renders the `apg/migration-guide.md` body for a layout upgrading from
/// `old_version` to `new_version` over `changelog` — the pure, unit-tested core
/// of [`write_migration_guide`]. It selects every entry with
/// `old_version < version <= new_version` that carries a `### Migration`
/// section, ascending, and concatenates them under a header naming the range;
/// when none does — or the layout records no prior version (a fresh
/// `apg init`) — the guide says so instead of listing tasks.
pub fn render_migration_guide(
    changelog: &str,
    old_version: Option<&str>,
    new_version: &str,
) -> String {
    let new = parse_semver(new_version).unwrap_or((u64::MAX, u64::MAX, u64::MAX));
    // A malformed old version is treated as the floor: the guide errs toward
    // listing the migration rather than silently skipping it.
    let old = old_version.map(|v| parse_semver(v).unwrap_or((0, 0, 0)));

    let entries = parse_changelog(changelog);
    let mut selected: Vec<&ChangelogEntry> = entries
        .iter()
        .filter(|e| {
            !e.migration.is_empty() && old.is_some_and(|o| e.version > o) && e.version <= new
        })
        .collect();
    selected.sort_by_key(|e| e.version);

    let mut out = String::from(
        "# apg migration guide\n\n\
         Written by `apg init`; read and worked by the **migration-agent**, which \
         finds it here in the repo's `apg/` dir.\n\n",
    );
    let Some(from) = old_version else {
        out.push_str(&format!(
            "No migration required: this layout records no prior version (a fresh \
             `apg init` at apg {new_version}), so there is nothing to migrate.\n"
        ));
        return out;
    };
    if selected.is_empty() {
        out.push_str(&format!(
            "No migration required: the changelog records no `### Migration` section \
             in the range ({from}, {new_version}] — no durable-spec change lands \
             between the layout version and apg {new_version}.\n"
        ));
        return out;
    }
    out.push_str(&format!(
        "The layout was at apg {from}; this binary is apg {new_version}. Run the \
         **migration-agent** to work the tasks below, then re-run `apg scan`.\n\n\
         ## Migration: {from} → {new_version}\n"
    ));
    for entry in selected {
        out.push_str(&format!(
            "\n### v{}\n\n{}\n",
            entry.version_text, entry.migration
        ));
    }
    out
}

/// Writes the guide [`render_migration_guide`] describes into
/// `apg/migration-guide.md`, best-effort — an unwritable `apg/` warns rather
/// than failing init (mirrors the upgrade-guide install). `old_version` is the
/// layout version captured **before** [`version_gate::ensure_config_version`]
/// rewrote it; `None` on a fresh init. Idempotent: an unchanged body leaves the
/// file alone.
fn write_migration_guide(apg_dir: &Path, old_version: Option<&str>) {
    let body = render_migration_guide(CHANGELOG, old_version, env!("CARGO_PKG_VERSION"));
    let path = apg_dir.join(MIGRATION_GUIDE);
    if let Err(e) = write_if_changed(&path, &body) {
        eprintln!(
            "warning: could not write {} ({e}); re-run `apg init` once the location is writable",
            path.display()
        );
    }
}

/// `apg init [dir]`: create the committed `apg/` layout (config.json carrying
/// the binary-managed layout `version` + `.trans/` + the project-worktrees
/// dir + the changelog-sliced `migration-guide.md`), install (or update) the
/// opencode apg tool suite + the eight
/// distributed agents + the upgrade guide into `~/.config/opencode/`, scaffold the
/// repo `.gitignore` for the apg layout entries (`apg/.trans/`,
/// `apg/.worktrees/`), and warn loudly about any project-local `.opencode/`
/// files that duplicate the installed suite (never deletes anything).
/// Project-specific implementer/reviewer agents are installed into the
/// project `.opencode/` by the agent-builder, not by init; that template
/// delivers the coordinator-mediated writer/reviewer grant shape — writers
/// hold the read-only `apg_review` channel and return a claim, and the
/// coordinator performs the shallow claim-vs-change check and then actions the
/// item — so a scaffolded reviewer never inherits writer-action wording. Init
/// is the layout's versioning/upgrade act (R9/R10): it re-runs idempotently
/// and writes the binary version into `apg/config.json` (user code_type rules
/// untouched) — `apg scan` and `apg project start` refuse to touch a layout
/// whose version is missing or does not share the binary's major.minor.
pub(crate) fn cmd_init(args: &[String]) -> anyhow::Result<()> {
    let dir = if args.is_empty() {
        std::env::current_dir()?
    } else {
        PathBuf::from(&args[0])
    };
    let dir = dir.canonicalize().unwrap_or_else(|_| dir.clone());

    let apg_dir = dir.join(specs::LAYOUT);
    std::fs::create_dir_all(apg_dir.join(specs::TRANS))?;
    // The project-worktrees dir (gitignored; `project start` nests each
    // project's worktree under it). Scaffolded even before git exists — the
    // dir itself is what libgit2's worktree add needs as a parent.
    std::fs::create_dir_all(apg_dir.join(git::WORKTREES))?;
    let cfg_path = apg_dir.join("config.json");
    if !cfg_path.exists() {
        std::fs::write(&cfg_path, DEFAULT_CONFIG_JSON)?;
    }
    // The binary-managed version field (R9): init is the layout's upgrade
    // act — write-through on every init, idempotent when already current.
    // Capture the PREVIOUS version first (before the write overwrites it): it
    // is the "from" bound of the migration guide written next.
    let old_version = version_gate::config_version(&apg_dir);
    // The changelog-sliced migration guide (best-effort): the migration-agent
    // reads it from the repo's apg/ dir and works the tasks for the delta. It
    // is written BEFORE the version bump below, so a failed guide write leaves
    // the layout on its old version — re-running `apg init` then re-slices the
    // same (old, new] range instead of a no-op that loses the tasks.
    write_migration_guide(&apg_dir, old_version.as_deref());
    version_gate::ensure_config_version(&apg_dir, env!("CARGO_PKG_VERSION"))?;

    let opencode_dir = user_opencode_dir()?;
    let (suite_files, agents, pruned) = install_suite(&opencode_dir)?;

    if !opencode_dir
        .join("node_modules")
        .join("@opencode")
        .join("plugin")
        .exists()
    {
        let status = Command::new("bun")
            .arg("install")
            .current_dir(&opencode_dir)
            .status();
        match status {
            Ok(s) if s.success() => {}
            _ => eprintln!(
                "warning: could not bun install in {}; run `bun install` there for the plugin import to resolve.",
                opencode_dir.display()
            ),
        }
    }

    if suite_files == 0 && agents == 0 {
        println!(
            "Initialized apg/ (config.json v{} + .trans/ + .worktrees/); {} suite files + {} agents already up to date in {}",
            env!("CARGO_PKG_VERSION"),
            SUITE_TOOLS.len() + 2,
            AGENTS.len(),
            opencode_dir.display()
        );
    } else {
        println!(
            "Initialized apg/ (config.json v{} + .trans/ + .worktrees/) and installed/updated {} of {} suite files + {} of {} agents in {}",
            env!("CARGO_PKG_VERSION"),
            suite_files,
            SUITE_TOOLS.len() + 2,
            agents,
            AGENTS.len(),
            opencode_dir.display()
        );
    }
    if pruned > 0 {
        println!(
            "pruned {} stale apg suite file(s) from {} (removed from the suite in a newer release)",
            pruned,
            opencode_dir.display()
        );
    }

    scaffold_gitignore(&dir)?;

    let project_opencode = dir.join(".opencode");
    let dupes = duplicate_install_files(&project_opencode, &opencode_dir);
    if !dupes.is_empty() {
        eprintln!(
            "!! WARNING: {} file(s) in {} duplicate the installed apg suite in {}:",
            dupes.len(),
            project_opencode.display(),
            opencode_dir.display()
        );
        for p in &dupes {
            eprintln!("     {}", p.strip_prefix(&dir).unwrap_or(p).display());
        }
        eprintln!(
            "   The project .opencode should hold only project-specific agents (installed by \
             agent-builder); these shadow the installed versions. Remove them if unintended."
        );
    }
    Ok(())
}

/// The apg-owned entries `apg init` scaffolds into the repo `.gitignore`:
/// the gitignored transient `apg/.trans/` (db, export, plans, renders, logs)
/// and the project worktrees dir `apg/.worktrees/` — one entry covers every
/// `<project>` nested under it (R9). Durable `apg/` data (config, specs,
/// notes) stays committed above both.
const GITIGNORE_APG_ENTRIES: &[&str] = &["apg/.trans/", "apg/.worktrees/"];

/// Ensures the repo `.gitignore` carries the apg layout entries (each added
/// if missing; other lines untouched), so the durable `apg/` data is
/// committed and only transient state (db, export, plans, renders, logs) and
/// project worktrees are ignored. `apg init` scaffolds both; `apg project
/// start` calls the same scaffolder to self-heal a repo whose entries were
/// dropped (it commits the change itself). Returns whether the file was
/// modified.
pub fn scaffold_gitignore(dir: &Path) -> anyhow::Result<bool> {
    let p = dir.join(".gitignore");
    let content = std::fs::read_to_string(&p).unwrap_or_default();
    let lines: Vec<&str> = content.lines().collect();
    let mut missing: Vec<&str> = Vec::new();
    for entry in GITIGNORE_APG_ENTRIES {
        // Both spellings count as present (git treats `dir/` and `dir`
        // equivalently for a directory entry).
        let bare = entry.trim_end_matches('/');
        let present = lines.iter().any(|l| l.trim().trim_end_matches('/') == bare);
        if !present {
            missing.push(entry);
        }
    }
    if missing.is_empty() {
        return Ok(false);
    }
    let mut out = content.clone();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(
        "# apg layout entries (transient/worktree dirs; committed apg/ data lives above them)\n",
    );
    for entry in &missing {
        out.push_str(entry);
        out.push('\n');
    }
    std::fs::write(&p, out)?;
    Ok(true)
}

#[cfg(test)]
mod tests;
