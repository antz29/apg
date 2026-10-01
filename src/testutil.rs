//! Shared test scaffolding (an unconditional public library module): real git
//! fixtures built entirely with git2 — the git CLI is never shelled out to
//! anywhere in src (R6), so fixture construction cannot use it either.
//!
//! It compiles unconditionally (not only under `cfg(test)`) so the relocated
//! e2e integration crates can reach it as `apg::testutil`; nothing here is
//! ever called by production code.
//!
//! R4 consequence: mutation-command tests no longer run against non-git
//! temp dirs — a write must happen inside a real project context (the
//! project's worktree at `<main>/apg/.worktrees/<project>`, on the project's
//! branch), so every fixture here is a small real repo with a real linked
//! worktree. The DB builders stay per-module (each test module needs its own
//! code graph); this module provides the git half plus the graph.jsonl
//! `scan_meta` writers every fixture needs to stay fresh.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use crate::graph::{Graph, Location, Node, NodeKind};
use crate::layers::{InEdge, NodeFile, OutEdge};
use crate::load;
use crate::schema;
use crate::specs;

/// Process-wide lock for tests that must mutate the process cwd (layout
/// discovery walks up from cwd). Shared by `scan_checkout` and any
/// CLI-dispatch test that temporarily chdirs into a fixture — concurrent
/// `set_current_dir` calls would otherwise interleave.
pub static CWD_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A fresh git repo (main checkout on branch `main`) with an initial commit
/// whose `.gitignore` covers `apg/.trans/` and `apg/.worktrees/`, and whose
/// `apg/config.json` carries the binary-managed `version` field at the
/// binary's own version — the R10 layout gate passes on every fixture, and
/// version-gate tests override the field explicitly.
pub struct Repo {
    /// The main checkout root.
    pub root: PathBuf,
}

pub fn git2_repo(repo: &Repo) -> git2::Repository {
    git2::Repository::open(&repo.root).expect("open fixture repo")
}

fn git_config(repo: &git2::Repository) {
    let mut cfg = repo.config().unwrap();
    cfg.set_str("user.name", "apg test").unwrap();
    cfg.set_str("user.email", "apg-test@example.com").unwrap();
}

impl Repo {
    /// Initializes the fixture repo: `.gitignore` + initial commit on `main`,
    /// plus the repo's own `apg/.trans/` layout marker (the walk-up layout
    /// discovery resolves the main checkout's layout root through it).
    pub fn new(tag: &str) -> Repo {
        let root = std::env::temp_dir().join(format!("apg-project-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let mut opts = git2::RepositoryInitOptions::new();
        opts.initial_head("refs/heads/main");
        let repo = git2::Repository::init_opts(&root, &opts).unwrap();
        git_config(&repo);
        std::fs::write(root.join(".gitignore"), "apg/.trans/\napg/.worktrees/\n").unwrap();
        std::fs::create_dir_all(root.join(specs::LAYOUT).join(specs::TRANS)).unwrap();
        // The versioned layout config (R10): the gate passes with the
        // binary's own major.minor; version-gate tests rewrite this field.
        std::fs::write(
            root.join(specs::LAYOUT).join("config.json"),
            format!(
                "{{\n  \"default\": \"src\",\n  \"types\": [],\n  \"version\": \"{}\"\n}}\n",
                env!("CARGO_PKG_VERSION")
            ),
        )
        .unwrap();
        let r = Repo { root };
        r.commit_all("init");
        r
    }

    /// The main checkout's `apg/` layout root (`<root>/apg`, created if
    /// missing — callers build their fixture layout under it).
    pub fn apg_root(&self) -> PathBuf {
        self.root.join(specs::LAYOUT)
    }

    /// The main checkout's current HEAD sha.
    pub fn head_sha(&self) -> String {
        let repo = git2_repo(self);
        repo.head()
            .unwrap()
            .peel_to_commit()
            .unwrap()
            .id()
            .to_string()
    }

    /// Writes `content` to `<root>/<rel>`.
    pub fn write(&self, rel: &str, content: &str) {
        let p = self.root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }

    /// Stages every change under the repo and commits on the current HEAD.
    /// Returns the new HEAD sha.
    pub fn commit_all(&self, msg: &str) -> String {
        let repo = git2_repo(self);
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = repo.signature().unwrap();
        let head = repo.head().ok().map(|h| h.peel_to_commit().unwrap());
        let parents: Vec<&git2::Commit> = head.iter().collect();
        let oid = repo
            .commit(Some("HEAD"), &sig, &sig, msg, &tree, parents.as_slice())
            .unwrap();
        oid.to_string()
    }

    /// True when `git status --porcelain` would be empty (tracked changes +
    /// untracked non-ignored files; ignored content — `.trans`, `.worktrees`
    /// — never counts).
    pub fn is_clean(&self) -> bool {
        let repo = git2_repo(self);
        let mut opts = git2::StatusOptions::new();
        opts.include_untracked(true);
        repo.statuses(Some(&mut opts)).unwrap().is_empty()
    }

    /// The canonical project worktree location for `project`.
    pub fn project_worktree_dir(&self, project: &str) -> PathBuf {
        self.root
            .join(specs::LAYOUT)
            .join(".worktrees")
            .join(project)
    }

    /// Creates a project context exactly like `apg project start` does (raw
    /// git2 — the production path is exercised by project_cmd's own tests):
    /// branch `project` off the main checkout's HEAD plus a linked worktree at
    /// `<main>/apg/.worktrees/<project>` with the branch checked out. Returns
    /// the worktree root. Callers then build their DB under the worktree's own
    /// `apg/`.
    ///
    /// libgit2's worktree add itself creates the branch (like `git worktree
    /// add <path>` creates the branch named after the last path component) at
    /// the main checkout's HEAD — a pre-existing branch of the same name is a
    /// hard error, mirroring the production collision refusals.
    pub fn start_project(&self, project: &str) -> PathBuf {
        let repo = git2_repo(self);
        let wt_path = self.project_worktree_dir(project);
        if let Some(parent) = wt_path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        repo.worktree(project, &wt_path, None).unwrap();
        let wt_repo = git2::Repository::open(&wt_path).unwrap();
        wt_repo.set_head(&format!("refs/heads/{project}")).unwrap();
        wt_repo
            .checkout_head(Some(&mut git2::build::CheckoutBuilder::new().force()))
            .unwrap();
        // Seed the worktree's own `apg/.trans/` marker — the marker production
        // start's scan copy provides — so walk-up layout discovery inside the
        // worktree resolves to the worktree's layout, not the main checkout's.
        std::fs::create_dir_all(wt_path.join(specs::LAYOUT).join(specs::TRANS)).unwrap();
        wt_path
    }

    /// The project worktree's `apg/` layout root.
    pub fn project_apg_root(&self, project: &str) -> PathBuf {
        self.project_worktree_dir(project).join(specs::LAYOUT)
    }
}

/// Writes a graph.jsonl whose line 1 is the scan_meta control record for a
/// scan at `sha`/`clean` (the real export writer). When a `db.lbug` already
/// exists, the DB's OWN `Scan` row is refreshed to the identical state, so the
/// fixture models a real scan's **two agreeing halves** — required by
/// [`crate::git::is_fresh`], which refuses a DB whose `Scan` row disagrees with
/// the export.
///
/// The content-identity key is taken from the checkout's CURRENT git state, so
/// a fixture that records the live sha/clean is genuinely fresh under the
/// phase-01 content rule. Use [`write_scan_meta_keyed`] to record an explicit
/// key (e.g. a stale or pre-hardening one).
pub fn write_scan_meta(apg_root: &Path, sha: Option<&str>, clean: bool, at: &str) {
    let key = crate::git::git_state(apg_root).content_key;
    write_scan_meta_keyed(apg_root, sha, clean, at, key.as_deref());
}

/// [`write_scan_meta`] with an explicit content-identity key (`None` models a
/// pre-hardening record whose freshness cannot be verified).
///
/// **Re-anchor, never truncate**: only line 1 (the `scan_meta` control record)
/// is replaced; every pre-existing export row — the code universe
/// `code_universes_from_export` reads and the authored→code edges
/// `tree_authored_identity` keeps — is preserved verbatim. A fixture with a
/// real code export therefore stays internally consistent across a re-anchor.
///
/// The DB half is written only when a `db.lbug` already exists (and opens): a
/// `graph.jsonl` without its DB is a legitimate fixture state, and
/// [`crate::git::is_fresh`] independently requires the live DB.
pub fn write_scan_meta_keyed(
    apg_root: &Path,
    sha: Option<&str>,
    clean: bool,
    at: &str,
    content_key: Option<&str>,
) {
    // Re-anchor ONLY the `scan_meta` control record on line 1, preserving every
    // pre-existing export row byte-for-byte (the code universe
    // `code_universes_from_export` reads, and the authored→code edges
    // `tree_authored_identity` keeps). A scan-meta-only rewrite would empty the
    // export's code set and silently drop those edges from the tree digest.
    let path = apg_root.join(specs::TRANS).join("graph.jsonl");
    let meta = serde_json::to_string(&schema::Record::ScanMeta {
        git_sha: sha.map(str::to_string),
        git_clean: sha.map(|_| clean),
        content_key: content_key.map(str::to_string),
        scanned_at: at.to_string(),
    })
    .unwrap();
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let mut body = String::with_capacity(meta.len() + existing.len() + 1);
    body.push_str(&meta);
    body.push('\n');
    for (i, line) in existing.lines().enumerate() {
        // Line 1 is the old scan_meta lead (if any) — dropped and replaced;
        // every other line (code/spec/edge rows) is preserved verbatim.
        if i == 0
            && serde_json::from_str::<schema::Record>(line.trim())
                .is_ok_and(|r| matches!(r, schema::Record::ScanMeta { .. }))
        {
            continue;
        }
        body.push_str(line);
        body.push('\n');
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&path, body).unwrap();

    // The DB's OWN `Scan` row — the other half of the recorded scan. Refresh it
    // to the identical state so `db_recorded_scan` and `recorded_scan` agree
    // (a real scan writes both together; `is_fresh` refuses a DB that only has
    // one half or whose halves disagree).
    if apg_root.join(specs::TRANS).join("db.lbug").exists()
        && let Ok(db) = crate::artifacts::ArtifactDb::open(apg_root)
    {
        db.refresh_scan_row(sha, sha.map(|_| clean), content_key)
            .unwrap();
    }
}

/// A real, empty `apg/.trans/db.lbug` with the full schema but no rows — the
/// DB-existence fixture the staleness predicates need (they open the DB to read
/// its `Scan` row; a raw empty marker is not a database). A test that wants the
/// DB to carry a `Scan` row pairs this with [`write_scan_meta`].
pub fn touch_db(apg_root: &Path) {
    let trans = apg_root.join(specs::TRANS);
    std::fs::create_dir_all(&trans).unwrap();
    let db = lbug::Database::new(trans.join("db.lbug"), Default::default()).unwrap();
    let conn = lbug::Connection::new(&db).unwrap();
    load::create_schema(&conn).unwrap();
}

/// Removes the fixture repo's temp dir (each test cleans up after itself).
pub fn remove(repo: &Repo) {
    let _ = std::fs::remove_dir_all(&repo.root);
}

/// A located `Graph` node fixture — the File/Struct/Function shape the
/// frontends emit, with a one-line span at `path`. A pure builder shared by
/// the unit/int and e2e tiers and single-sourced here per
/// `solution.constraint.test-layout-shape`; unlike the I/O harness below it
/// performs no I/O and may be used by any tier.
pub fn located(kind: NodeKind, path: &str) -> Node {
    Node {
        kind,
        location: Some(Location {
            path: PathBuf::from(path),
            start: 0,
            end: 1,
            start_line: 1,
            end_line: 1,
        }),
        ..Node::default()
    }
}

/// A bare `layers::NodeFile` with no edges — the identity fields set, body
/// empty — for building node-file/edge fixtures. A pure builder shared by the
/// unit/int and e2e tiers and single-sourced here per
/// `solution.constraint.test-layout-shape`; it performs no I/O and may be used
/// by any tier.
pub fn node(layer: &str, node_type: &str, name: &str) -> NodeFile {
    NodeFile {
        layer: layer.to_string(),
        node_type: node_type.to_string(),
        name: name.to_string(),
        body: String::new(),
        properties: BTreeMap::new(),
        out: Vec::new(),
        in_edges: Vec::new(),
    }
}

/// A bare `layers::OutEdge` with no properties — a pure builder shared by the
/// unit/int and e2e tiers.
pub fn out_edge(kind: &str, target: &str) -> OutEdge {
    OutEdge {
        kind: kind.to_string(),
        target: target.to_string(),
        properties: BTreeMap::new(),
    }
}

/// A bare `layers::InEdge` with no properties — a pure builder shared by the
/// unit/int and e2e tiers.
pub fn in_edge(kind: &str, source: &str) -> InEdge {
    InEdge {
        kind: kind.to_string(),
        source: source.to_string(),
        properties: BTreeMap::new(),
    }
}

/// A caller-supplied code-FQN universe: the scanned set or the planned set,
/// both plain [`BTreeSet`]s of opaque FQN strings. A pure builder shared by the
/// unit/int and e2e tiers.
pub fn code_universe(fqns: &[&str]) -> BTreeSet<String> {
    fqns.iter().map(|s| s.to_string()).collect()
}

/// Reads `graph.jsonl` back into a [`Graph`] — the re-ingest leg of the
/// export round-trip (PHASE_01 done gate: "JSONL → DB → JSONL round-trip for
/// each" node/edge kind). Mirror of `crate::load::write_graph_jsonl`: every
/// `Export` record line is mapped back to the graph node or edge it came from.
/// A `scan_meta` control record on line 1 reconstructs the `Scan` node at
/// `SCAN_HEAD` (it is never emitted as a node line). Unknown `type`s are an
/// error so a new export kind cannot silently vanish on the way back in.
///
/// Shared by the relocated e2e crates and the root module tree's own tests
/// (single-sourced here per `solution.constraint.test-layout-shape`).
pub fn read_graph_jsonl(path: &Path) -> anyhow::Result<Graph> {
    let text = std::fs::read_to_string(path)?;
    let mut g = Graph::default();

    for line in text.lines() {
        let v: serde_json::Value = serde_json::from_str(line)?;
        let t = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
        let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
        let o = |k: &str| v.get(k).and_then(|x| x.as_str()).map(str::to_string);
        let u = |k: &str| v.get(k).and_then(|x| x.as_u64()).unwrap_or(0) as u32;
        let located = || {
            Some(Location {
                path: s("path").into(),
                start: u("start"),
                end: u("end"),
                start_line: u("start_line"),
                end_line: u("end_line"),
            })
        };
        match t {
            "scan_meta" => {
                let mut n = Node {
                    kind: NodeKind::Scan,
                    ..Node::default()
                };
                n.git_sha = o("git_sha");
                n.git_clean = v.get("git_clean").and_then(|x| x.as_bool());
                n.content_key = o("content_key");
                n.scanned_at = o("scanned_at");
                g.nodes.insert(crate::schema::SCAN_HEAD.to_string(), n);
            }
            "language" => {
                // A language-root node (`lang_switch` id, e.g. `rust`) carries
                // only its fqn — no location, no code_type; this mirrors
                // `Export::Language` (PHASE_09 language rooting) so the record
                // round-trips instead of hitting the unknown-type bail.
                g.nodes.insert(
                    s("fqn"),
                    Node {
                        kind: NodeKind::Language,
                        ..Node::default()
                    },
                );
            }
            "module" => {
                g.nodes.insert(
                    s("fqn"),
                    Node {
                        kind: NodeKind::Module,
                        status: o("status"),
                        ..Node::default()
                    },
                );
            }
            "struct" => {
                let fqn = s("fqn");
                g.nodes.insert(
                    fqn,
                    Node {
                        kind: NodeKind::Struct,
                        location: located(),
                        code_type: s("code_type"),
                        status: o("status"),
                        ..Node::default()
                    },
                );
            }
            "function" => {
                let fqn = s("fqn");
                g.nodes.insert(
                    fqn,
                    Node {
                        kind: NodeKind::Function,
                        location: located(),
                        code_type: s("code_type"),
                        status: o("status"),
                        ..Node::default()
                    },
                );
            }
            "file" => {
                // A File node's fqn IS its absolute path (no separate path
                // field is exported); the line range rides as the span.
                let fqn = s("fqn");
                g.nodes.insert(
                    fqn.clone(),
                    Node {
                        kind: NodeKind::File,
                        location: Some(Location {
                            path: fqn.into(),
                            start: 0,
                            end: 0,
                            start_line: u("start_line"),
                            end_line: u("end_line"),
                        }),
                        code_type: s("code_type"),
                        status: o("status"),
                        ..Node::default()
                    },
                );
            }
            "unresolved" => {
                g.nodes.insert(
                    s("fqn"),
                    Node {
                        kind: NodeKind::UnresolvedTarget,
                        category: o("category"),
                        ..Node::default()
                    },
                );
            }
            "requirement" => {
                g.nodes.insert(
                    s("fqn"),
                    Node {
                        kind: NodeKind::Requirement,
                        id: o("id"),
                        title: o("title"),
                        body: o("body"),
                        feature: o("feature"),
                        ..Node::default()
                    },
                );
            }
            "note" => {
                g.nodes.insert(
                    s("fqn"),
                    Node {
                        kind: NodeKind::Note,
                        body: o("body"),
                        sub_kind: o("kind"),
                        ..Node::default()
                    },
                );
            }
            "feedback" => {
                g.nodes.insert(
                    s("fqn"),
                    Node {
                        kind: NodeKind::Feedback,
                        body: o("body"),
                        status: o("status"),
                        disposition: o("disposition"),
                        ..Node::default()
                    },
                );
            }
            "plan" => {
                g.nodes.insert(
                    s("fqn"),
                    Node {
                        kind: NodeKind::Plan,
                        title: o("title"),
                        strategy: o("strategy"),
                        ..Node::default()
                    },
                );
            }
            "plan_phase" => {
                g.nodes.insert(
                    s("fqn"),
                    Node {
                        kind: NodeKind::PlanPhase,
                        number: v.get("number").and_then(|x| x.as_u64()).map(|x| x as u32),
                        title: o("title"),
                        deliverable: o("deliverable"),
                        status: o("status"),
                        ..Node::default()
                    },
                );
            }
            "task" => {
                g.nodes.insert(
                    s("fqn"),
                    Node {
                        kind: NodeKind::Task,
                        title: o("title"),
                        sub_kind: o("kind"),
                        tier: o("tier"),
                        status: o("status"),
                        verb: o("verb"),
                        target: o("target"),
                        new_fqn: o("new_fqn"),
                        ..Node::default()
                    },
                );
            }
            "stakeholder" => {
                g.nodes.insert(
                    s("fqn"),
                    Node {
                        kind: NodeKind::Stakeholder,
                        name: o("name"),
                        body: o("body"),
                        ..Node::default()
                    },
                );
            }
            "entity" => {
                g.nodes.insert(
                    s("fqn"),
                    Node {
                        kind: NodeKind::Entity,
                        name: o("name"),
                        body: o("body"),
                        ..Node::default()
                    },
                );
            }
            "system" => {
                g.nodes.insert(
                    s("fqn"),
                    Node {
                        kind: NodeKind::System,
                        name: o("name"),
                        body: o("body"),
                        ..Node::default()
                    },
                );
            }
            "container" => {
                g.nodes.insert(
                    s("fqn"),
                    Node {
                        kind: NodeKind::Container,
                        name: o("name"),
                        sub_kind: o("kind"),
                        body: o("body"),
                        ..Node::default()
                    },
                );
            }
            "component" => {
                g.nodes.insert(
                    s("fqn"),
                    Node {
                        kind: NodeKind::Component,
                        name: o("name"),
                        body: o("body"),
                        ..Node::default()
                    },
                );
            }
            "user" => {
                g.nodes.insert(
                    s("fqn"),
                    Node {
                        kind: NodeKind::User,
                        name: o("name"),
                        body: o("body"),
                        ..Node::default()
                    },
                );
            }
            "group" => {
                g.nodes.insert(
                    s("fqn"),
                    Node {
                        kind: NodeKind::Group,
                        name: o("name"),
                        attribute: o("attribute"),
                        root: o("root"),
                        body: o("body"),
                        ..Node::default()
                    },
                );
            }
            "value" => {
                g.nodes.insert(
                    s("fqn"),
                    Node {
                        kind: NodeKind::Value,
                        name: o("name"),
                        body: o("body"),
                        ..Node::default()
                    },
                );
            }
            "service" => {
                g.nodes.insert(
                    s("fqn"),
                    Node {
                        kind: NodeKind::Service,
                        name: o("name"),
                        body: o("body"),
                        ..Node::default()
                    },
                );
            }
            "person" => {
                g.nodes.insert(
                    s("fqn"),
                    Node {
                        kind: NodeKind::Person,
                        name: o("name"),
                        body: o("body"),
                        ..Node::default()
                    },
                );
            }
            "constraint" => {
                g.nodes.insert(
                    s("fqn"),
                    Node {
                        kind: NodeKind::Constraint,
                        name: o("name"),
                        body: o("body"),
                        attaches_to: o("attaches-to"),
                        ..Node::default()
                    },
                );
            }
            "contains" => {
                g.contains.insert((s("from"), s("to")));
            }
            "calls" => {
                g.calls.insert((s("from"), s("to")));
            }
            "uses" => {
                g.uses.insert((s("from"), s("to")));
            }
            "unresolved_call" => {
                g.unresolved_calls
                    .insert((s("from"), s("to"), s("target_type")));
            }
            "unresolved_use" => {
                g.unresolved_uses.insert((s("from"), s("to")));
            }
            "details" => {
                g.details.insert((s("from"), s("to")));
            }
            "reviews" => {
                g.reviews.insert((s("from"), s("to")));
            }
            "depends_on" => {
                g.depends_on.insert((s("from"), s("to")));
            }
            "gates" => {
                g.gates.insert((s("from"), s("to")));
            }
            "satisfies" => {
                g.satisfies.insert((s("from"), s("to")));
            }
            "drives" => {
                g.drives.insert((s("from"), s("to")));
            }
            "represents" => {
                g.represents.insert((s("from"), s("to")));
            }
            "realised-by" => {
                g.realised_by.insert((s("from"), s("to")));
            }
            "implemented-by" => {
                g.spec_implemented_by.insert((s("from"), s("to")));
            }
            "publishes" => {
                g.publishes.insert((s("from"), s("to")));
            }
            "subscribes" => {
                g.subscribes.insert((s("from"), s("to")));
            }
            other => {
                anyhow::bail!("graph.jsonl record with unknown type `{other}`");
            }
        }
    }
    Ok(g)
}

// ---------------------------------------------------------------------------
// Cross-process CLI harness: concurrency and read-your-writes tests must drive
// N separate `apg` processes, not in-process command calls. In-process calls
// share process-wide state — above all the `specs.lock` flock (`SPEC_LOCK` is a
// process-lifetime `OnceLock`) and the one lbug `Database` handle — so an
// in-process "burst" false-greens exactly the lost-update race these tests
// exist to catch.
// ---------------------------------------------------------------------------

/// Resolves the built `apg` binary this test process drives as a separate CLI.
///
/// Unit tests execute inside the *test-harness* binary
/// (`target/<profile>/deps/apg-<hash>`), so `current_exe()` does not name the
/// CLI, and Cargo only sets `CARGO_BIN_EXE_apg` for integration tests. The
/// resolution order is:
///
/// 1. the compile-time `CARGO_BIN_EXE_apg`, when Cargo provided it;
/// 2. the `apg` sibling of the profile dir, found by walking up from
///    `current_exe()` (`target/<profile>/deps/apg-<hash>` →
///    `target/<profile>/apg`) — the artifact `cargo build` leaves.
///
/// Panics with the search origin when neither resolves: run `cargo build`
/// first.
pub fn apg_bin() -> PathBuf {
    if let Some(p) = option_env!("CARGO_BIN_EXE_apg") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return p;
        }
    }
    let exe = std::env::current_exe().expect("current_exe");
    let name = if cfg!(windows) { "apg.exe" } else { "apg" };
    let mut dir = exe.parent();
    while let Some(d) = dir {
        let candidate = d.join(name);
        if candidate.is_file() {
            return candidate;
        }
        dir = d.parent();
    }
    panic!(
        "could not locate the built `apg` binary from {} — run `cargo build` first",
        exe.display()
    );
}

/// A configured real-CLI `apg` invocation: the built binary at [`apg_bin`],
/// the argument list, an optional cwd, and per-child environment overrides.
///
/// `edition = "2024"` makes process-wide `std::env::set_var` unsafe, so a child
/// that needs an isolated environment (e.g. `apg init` installing into
/// `$HOME/.opencode`) sets it per child with [`ApgCommand::env`] — never on the
/// test process, where it would leak into every parallel test.
pub struct ApgCommand {
    bin: PathBuf,
    args: Vec<String>,
    cwd: Option<PathBuf>,
    envs: Vec<(String, String)>,
}

impl ApgCommand {
    pub fn new(args: &[&str]) -> ApgCommand {
        ApgCommand {
            bin: apg_bin(),
            args: args.iter().map(|s| s.to_string()).collect(),
            cwd: None,
            envs: Vec::new(),
        }
    }

    pub fn cwd(mut self, dir: &Path) -> ApgCommand {
        self.cwd = Some(dir.to_path_buf());
        self
    }

    /// A per-child environment override set on the child's `Command`
    /// (`Command::env`), the safe way to isolate a spawned CLI.
    pub fn env(mut self, key: &str, value: &str) -> ApgCommand {
        self.envs.push((key.to_string(), value.to_string()));
        self
    }

    /// The configured `Command`: stdout/stderr captured, so a test can
    /// attribute a failure's lock by message and N parallel children never
    /// interleave output into the harness log.
    pub fn command(self) -> Command {
        let mut c = Command::new(&self.bin);
        c.args(&self.args);
        if let Some(dir) = &self.cwd {
            c.current_dir(dir);
        }
        for (k, v) in &self.envs {
            c.env(k, v);
        }
        c.stdout(Stdio::piped()).stderr(Stdio::piped());
        c
    }

    /// Starts the child without waiting — the N-way parallel-burst primitive.
    pub fn spawn(self) -> Child {
        let desc = format!("{:?}", self.args);
        self.command()
            .spawn()
            .unwrap_or_else(|e| panic!("failed to spawn apg {desc}: {e}"))
    }

    /// Runs to completion and returns the captured [`Output`].
    pub fn output(self) -> Output {
        let desc = format!("{:?}", self.args);
        self.command()
            .output()
            .unwrap_or_else(|e| panic!("failed to spawn apg {desc}: {e}"))
    }
}

/// Spawns one real `apg` CLI process with `args` and cwd `dir`, waits for it,
/// and returns its captured [`Output`] — a genuinely separate process, so
/// process-wide state (the `specs.lock` flock, the lbug database handle, git's
/// index lock) is exercised for real. Use [`ApgCommand`] for a per-child env
/// override or a parallel [`ApgCommand::spawn`].
pub fn spawn_apg(args: &[&str], cwd: &Path) -> Output {
    ApgCommand::new(args).cwd(cwd).output()
}

// ---------------------------------------------------------------------------
// Session harness (phase-03): drive a real long-running `apg session start`
// process. The coordinator must be a genuinely separate process — it owns the
// DB and the extended flock for its life — so session tests spawn it, wait
// until its socket answers, route mutations/reads through it, and end it (or
// SIGKILL it, for the crash/reclaim case).
// ---------------------------------------------------------------------------

/// A live `apg session start` process under test.
pub struct SessionProcess {
    pub child: Child,
}

/// Spawns `apg session start` in `wt` with an isolated `HOME`, then waits until
/// the session socket answers (or panics, dumping the child's output).
pub fn start_session_process(wt: &Path, home: &Path) -> SessionProcess {
    std::fs::create_dir_all(home).unwrap();
    let apg_root = specs::find_apg_root(wt).expect("fixture layout root");
    let mut child = ApgCommand::new(&["session", "start"])
        .cwd(wt)
        .env("HOME", home.to_str().unwrap())
        .spawn();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if crate::session::live_session(&apg_root) {
            return SessionProcess { child };
        }
        if let Some(status) = child.try_wait().unwrap() {
            let out = child.wait_with_output().unwrap();
            panic!(
                "session exited early ({status}): {}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let out = child.wait_with_output().unwrap();
            panic!(
                "session did not become live: {}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Commits `rels` on `wt`'s current branch (git2 — the same mechanics
/// `git::commit_files` uses), returning the new branch HEAD sha. The sha is
/// the handle a caller re-anchors `scan_meta` against so the branch graph
/// stays fresh; callers that only need the commit still discard the return.
pub fn wt_commit(wt: &Path, rels: &[&str], msg: &str) -> String {
    let repo = git2::Repository::open(wt).unwrap();
    let mut index = repo.index().unwrap();
    for rel in rels {
        index.add_path(Path::new(rel)).unwrap();
    }
    index.write().unwrap();
    let tree_id = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    let sig = repo.signature().unwrap();
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, msg, &tree, &[&head])
        .unwrap()
        .to_string()
}

/// Commits `rels` on `wt`'s current branch (git2) and returns the new branch
/// HEAD sha. Unlike [`wt_commit`] it stages through the index pathspec matcher,
/// so `rels` may name a directory or a git pathspec — the superset needed by
/// test setup that deliberately writes outside the commit funnel, before a
/// scan_meta re-anchor. The single canonical home for the relocated e2e
/// crates' `wt_commit_paths` fixtures.
pub fn wt_commit_paths(wt: &Path, rels: &[&str], msg: &str) -> String {
    let repo = git2::Repository::open(wt).unwrap();
    let mut index = repo.index().unwrap();
    index
        .add_all(rels.iter().copied(), git2::IndexAddOption::DEFAULT, None)
        .unwrap();
    index.write().unwrap();
    let tree_id = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    let sig = repo.signature().unwrap();
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, msg, &tree, &[&head])
        .unwrap()
        .to_string()
}

/// The number of commits reachable from the checkout's HEAD — used to assert
/// one-commit-per-mutation (and no commit on an at-most-once replay).
pub fn commit_count(dir: &Path) -> usize {
    let repo = git2::Repository::open(dir).unwrap();
    let mut walk = repo.revwalk().unwrap();
    walk.push_head().unwrap();
    walk.count()
}

/// A real project worktree carrying a code graph: main repo + worktree `foo` on
/// branch `foo` with a committed source payload scanned into
/// `<wt>/apg/.trans/db.lbug`. Returns `(repo, wt, wt_apg)` — the state a
/// session coordinator owns.
pub fn project_with_db(tag: &str) -> (Repo, PathBuf, PathBuf) {
    let repo = Repo::new(tag);
    let wt = repo.start_project("foo");
    let wt_apg = wt.join(specs::LAYOUT);
    let seed = wt.join("code/seed.scan.jsonl");
    std::fs::create_dir_all(seed.parent().unwrap()).unwrap();
    std::fs::write(
        &seed,
        code_payload("fixture.mod", "/abs/store.go", &["Store"]),
    )
    .unwrap();
    wt_commit(&wt, &["code/seed.scan.jsonl"], "seed code");
    scan_checkout(&wt).unwrap();
    (repo, wt, wt_apg)
}

// ---------------------------------------------------------------------------
// Hermetic scans: tests never spawn frontends, so a fixture checkout's "code"
// is whatever `*.scan.jsonl` payload files it carries (scanner-shaped records,
// piped through the real ingest pipeline exactly like a frontend spool), plus
// its `apg/layers` node-file tree and `apg/.trans/` plan store + feedback
// mirrors (the same record stream `cmd_scan` assembles).
// ---------------------------------------------------------------------------

/// A scanner-style module record line.
pub fn module_line(fqn: &str) -> String {
    format!("{{\"type\":\"module\",\"fqn\":\"{fqn}\"}}")
}

/// A scanner-style file record line.
pub fn file_line(path: &str, parent: &str, end_line: u32) -> String {
    format!(
        "{{\"type\":\"file\",\"path\":\"{path}\",\"parent\":\"{parent}\",\"start_line\":1,\"end_line\":{end_line}}}"
    )
}

/// A scanner-style struct record line.
pub fn struct_line(id: &str, parent: &str, name: &str, path: &str) -> String {
    format!(
        "{{\"type\":\"struct\",\"id\":\"{id}\",\"parent\":\"{parent}\",\"name\":\"{name}\",\"path\":\"{path}\",\"start\":1,\"end\":10,\"start_line\":1,\"end_line\":10}}"
    )
}

/// A scanner-style function record line (unique in its parent scope — no
/// params — so the ingestor renders the plain `parent.name` FQN).
pub fn function_line(id: &str, parent: &str, name: &str, path: &str) -> String {
    format!(
        "{{\"type\":\"function\",\"id\":\"{id}\",\"parent\":\"{parent}\",\"name\":\"{name}\",\"params\":[],\"file\":\"{path}\",\"path\":\"{path}\",\"start\":1,\"end\":5,\"start_line\":1,\"end_line\":5}}"
    )
}

/// A ready-made payload for one fixture "module": module + structs + file
/// (containment is derived ingestor-side from `parent`/`path`, like the real
/// scanner streams).
pub fn code_payload(module: &str, file: &str, structs: &[&str]) -> String {
    let mut s = String::new();
    s.push_str(&module_line(module));
    s.push('\n');
    for (i, st) in structs.iter().enumerate() {
        s.push_str(&struct_line(&format!("n{}", i + 1), module, st, file));
        s.push('\n');
    }
    s.push_str(&file_line(file, module, 100));
    s.push('\n');
    s
}

/// Every `*.scan.jsonl` payload under `dir` (sorted), skipping nested
/// checkouts and dependency dirs (`.git`, `.worktrees`, `target`,
/// `node_modules`).
pub fn payload_files(dir: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                if matches!(
                    p.file_name().and_then(|n| n.to_str()),
                    Some(".git" | ".worktrees" | "target" | "node_modules")
                ) {
                    continue;
                }
                walk(&p, out);
            } else if p
                .file_name()
                .is_some_and(|n| n.to_string_lossy().ends_with(".scan.jsonl"))
            {
                out.push(p);
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, &mut out);
    out.sort();
    out
}

/// Runs `f` with the process cwd temporarily set to `dir`, restoring it after.
/// The single canonical home for the relocated e2e crates' cwd-scoped fixtures
/// (`apg::testutil::with_cwd`); serialized behind [`CWD_LOCK`] so a chdir never
/// interleaves with [`scan_checkout`], which also mutates the process-global
/// cwd.
pub fn with_cwd<T>(dir: &Path, f: impl FnOnce() -> T) -> T {
    let _guard = CWD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let old = std::env::current_dir().unwrap();
    std::env::set_current_dir(dir).unwrap();
    let out = f();
    std::env::set_current_dir(old).unwrap();
    out
}

/// The `Vec<String>` CLI-argv shape the node/plan command dispatchers take
/// (positionals + repeatable flags). A pure builder used by BOTH the unit/int
/// tier and the e2e tier of `node_cmd` (and the sibling `plan_cmd` relocation),
/// so it has exactly one definition here per
/// `solution.constraint.test-layout-shape`.
pub fn av(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| s.to_string()).collect()
}

/// A layers `NodeFile` with the given identity and `(kind, target)` out-edges
/// — the pure builder for `plan_cmd`'s coverage/spine fixtures. Used by BOTH
/// the unit/int tier (sibling `src/plan_cmd/tests.rs`) and the relocated e2e
/// crate, so it is single-sourced here per
/// `solution.constraint.test-layout-shape`.
pub fn nf(
    layer: &str,
    node_type: &str,
    name: &str,
    edges: &[(&str, &str)],
) -> crate::layers::NodeFile {
    crate::layers::NodeFile {
        layer: layer.to_string(),
        node_type: node_type.to_string(),
        name: name.to_string(),
        body: String::new(),
        properties: std::collections::BTreeMap::new(),
        out: edges
            .iter()
            .map(|(k, t)| crate::layers::OutEdge {
                kind: k.to_string(),
                target: t.to_string(),
                properties: std::collections::BTreeMap::new(),
            })
            .collect(),
        in_edges: Vec::new(),
    }
}

/// A single-task plan record (`<project>/plan.phase-01.task-1`) with the given
/// verb and target(s) — the coverage touch source. Shared by `plan_cmd`'s
/// unit/int and e2e tiers, single-sourced here per
/// `solution.constraint.test-layout-shape`.
pub fn task_rec(verb: &str, target: &str, new_fqn: &str) -> crate::schema::Record {
    crate::schema::Record::Task {
        fqn: "foo/plan.phase-01.task-1".to_string(),
        title: "T".to_string(),
        kind: "source".to_string(),
        tier: String::new(),
        status: "pending".to_string(),
        verb: verb.to_string(),
        target: target.to_string(),
        new_fqn: new_fqn.to_string(),
    }
}

/// A hermetic stand-in for `apg scan` (tests never spawn frontends): pipes the
/// checkout's `*.scan.jsonl` payloads through the real ingest pipeline, then
/// chains the same post-code leg `cmd_scan` assembles — the durable
/// `apg/layers` tree via `layers::ingest_tree` plus the transient `.trans/`
/// plan store and feedback tier mirrors — writing `db.lbug` + `graph.jsonl`
/// into the checkout's own `apg/.trans/`. The `scan_meta` records the real git
/// state of the checkout, so staleness behaves exactly like a real scan.
///
/// `run_pipeline` writes relative to the process cwd, so every scan is
/// serialized behind a process-wide lock: concurrent scans (tests run in
/// parallel) must never interleave their `set_current_dir`.
pub fn scan_checkout(project_dir: &Path) -> anyhow::Result<()> {
    let _guard = CWD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    scan_checkout_locked(project_dir)
}

/// [`scan_checkout`] for a caller that already holds [`CWD_LOCK`]. The scan
/// mutates the process-global cwd (`run_pipeline` chdirs into `.trans`), so the
/// caller must guarantee exclusivity itself. A test that must run several scans
/// folds them into ONE lock hold through this entry point, so it never
/// re-queues on the shared lock between them (the re-queueing, not the scan
/// itself, is what pushes a multi-scan test past libtest's 60s warning).
///
/// # Safety contract
/// Call only while [`CWD_LOCK`] is held; concurrent use would interleave
/// `set_current_dir` and corrupt the other scan's view of the cwd.
pub fn scan_checkout_locked(project_dir: &Path) -> anyhow::Result<()> {
    let apg_root = specs::find_or_create_apg_root(project_dir);
    let trans_dir = apg_root.join(specs::TRANS);
    std::fs::create_dir_all(&trans_dir)?;
    let state = crate::git::git_state(&apg_root);

    // The scanner-shaped records (scan_meta + payloads) — the same stream
    // `cmd_scan` builds, kept separate so it can be pre-ingested for the
    // scanned code-FQN universe `ingest_tree` validates against.
    let mut scanner_records: Vec<crate::schema::Record> = Vec::new();
    scanner_records.push(crate::schema::Record::ScanMeta {
        git_sha: state.sha.clone(),
        git_clean: state.sha.as_ref().map(|_| state.clean),
        content_key: state.content_key.clone(),
        scanned_at: crate::git::now_iso8601(),
    });
    for p in payload_files(project_dir) {
        scanner_records.push(crate::schema::Record::LangSwitch {
            language: "go".to_string(),
        });
        let text = std::fs::read_to_string(&p)?;
        for (i, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let rec: crate::schema::Record = serde_json::from_str(line).map_err(|e| {
                anyhow::anyhow!("{}:{}: bad payload record: {e}\n{line}", p.display(), i + 1)
            })?;
            scanner_records.push(rec);
        }
    }

    // Pre-ingest the scanner stream to compute the scanned code-FQN universe
    // (mirrors cmd_scan).
    let scanned_code: std::collections::BTreeSet<String> = {
        let (pre, _) = crate::ingest::ingest(
            scanner_records.clone(),
            &crate::ingest::IngestOptions {
                blacklist: &[],
                language: "go",
                config: None,
                base: None,
            },
        );
        pre.nodes
            .iter()
            .filter(|(_, n)| {
                matches!(
                    n.kind,
                    crate::graph::NodeKind::Module
                        | crate::graph::NodeKind::Struct
                        | crate::graph::NodeKind::Function
                        | crate::graph::NodeKind::File
                ) && n.status.is_none()
            })
            .map(|(f, _)| f.clone())
            .collect()
    };

    // The transient legs (`.trans/plans/*.jsonl` — the plan store — plus the
    // five `.trans/<tier>/*.jsonl` feedback mirrors, SPEC §5) — the committed
    // spec/note durable halves are gone; spec data comes from the `apg/layers`
    // tree via `layers::ingest_tree`.
    let mut transient_records: Vec<crate::schema::Record> = Vec::new();
    let mut planned: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let transient_files = crate::specs::plan_files(&apg_root)
        .into_iter()
        .chain(crate::specs::trans_mirror_files(&apg_root))
        .collect::<Vec<_>>();
    for f in transient_files {
        for r in crate::specs::read_jsonl(&f).unwrap_or_else(|e| panic!("{e:#}")) {
            if let crate::schema::Record::PlannedNode { fqn, .. } = &r {
                planned.insert(fqn.clone());
            }
            transient_records.push(r);
        }
    }

    // Ingest the durable `apg/layers` tree into new-model records (mirrors
    // cmd_scan), validating pairing/code-refs/constraints against the scanned
    // code and the planned-node universe.
    let layers_records = crate::layers::ingest_tree(&apg_root, &scanned_code, &planned)?;

    let records = scanner_records
        .into_iter()
        .chain(layers_records)
        .chain(transient_records);

    let old = std::env::current_dir()?;
    std::env::set_current_dir(&trans_dir)?;
    let result = {
        let mut log = crate::Log::new();
        crate::run_pipeline(records, &[], &[], "go", None, None, None, &mut log);
        Ok(())
    };
    std::env::set_current_dir(old)?;
    result
}

/// Recursively copies a directory (the isolated Java frontend staging).
pub fn copy_dir(src: &Path, dst: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &to)?;
        } else {
            std::fs::copy(entry.path(), &to)?;
        }
    }
    Ok(())
}
