//! Shared test scaffolding (compiled only under `cargo test`): real git
//! fixtures built entirely with git2 — the git CLI is never shelled out to
//! anywhere in src (R6), so fixture construction cannot use it either.
//!
//! R4 consequence: mutation-command tests no longer run against non-git
//! temp dirs — a write must happen inside a real project context (the
//! project's worktree at `<main>/apg/.worktrees/<project>`, on the project's
//! branch), so every fixture here is a small real repo with a real linked
//! worktree. The DB builders stay per-module (each test module needs its own
//! code graph); this module provides the git half plus the graph.jsonl
//! `scan_meta` writers every fixture needs to stay fresh.

#![cfg(test)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};

use crate::graph::{Graph, Node, NodeKind};
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
        // Seed the worktree's own `apg/.trans/` marker — exactly what the
        // production start does before its auto-scan — so walk-up layout
        // discovery inside the worktree resolves to the worktree's layout,
        // not the main checkout's.
        std::fs::create_dir_all(wt_path.join(specs::LAYOUT).join(specs::TRANS)).unwrap();
        wt_path
    }

    /// The project worktree's `apg/` layout root.
    pub fn project_apg_root(&self, project: &str) -> PathBuf {
        self.project_worktree_dir(project).join(specs::LAYOUT)
    }
}

/// Writes a graph.jsonl whose line 1 is the scan_meta control record for a
/// scan at `sha`/`clean` (the real export writer; the DB Scan node is written
/// by the same `load` path the fixture DB builders use).
pub fn write_scan_meta(apg_root: &Path, sha: Option<&str>, clean: bool, at: &str) {
    let mut g = Graph::default();
    g.nodes.insert(
        schema::SCAN_HEAD.to_string(),
        Node {
            kind: NodeKind::Scan,
            git_sha: sha.map(str::to_string),
            git_clean: sha.map(|_| clean),
            scanned_at: Some(at.to_string()),
            ..Node::default()
        },
    );
    load::write_graph_jsonl(&g, &apg_root.join(specs::TRANS).join("graph.jsonl")).unwrap();
}

/// An empty `apg/.trans/db.lbug` marker (staleness predicates only check DB
/// existence). Full DBs are built by each test module's own fixture over the
/// same layout.
pub fn touch_db(apg_root: &Path) {
    std::fs::create_dir_all(apg_root.join(specs::TRANS)).unwrap();
    std::fs::write(apg_root.join(specs::TRANS).join("db.lbug"), "").unwrap();
}

/// Removes the fixture repo's temp dir (each test cleans up after itself).
pub fn remove(repo: &Repo) {
    let _ = std::fs::remove_dir_all(&repo.root);
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
        crate::run_pipeline(records, &[], &[], "go", None, &mut log);
        Ok(())
    };
    std::env::set_current_dir(old)?;
    result
}
