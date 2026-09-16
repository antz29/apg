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

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

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
pub fn write_scan_meta_keyed(
    apg_root: &Path,
    sha: Option<&str>,
    clean: bool,
    at: &str,
    content_key: Option<&str>,
) {
    let mut g = Graph::default();
    g.nodes.insert(
        schema::SCAN_HEAD.to_string(),
        Node {
            kind: NodeKind::Scan,
            git_sha: sha.map(str::to_string),
            git_clean: sha.map(|_| clean),
            content_key: content_key.map(str::to_string),
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

/// The `apg` binary file name for the host platform.
fn apg_bin_name() -> &'static str {
    if cfg!(windows) { "apg.exe" } else { "apg" }
}

/// The cargo `target/` directory for this checkout — the parent of the candidate
/// profile dirs. `CARGO_TARGET_DIR` wins when set, else `<manifest>/target`.
pub fn acceptance_target_dir() -> Option<PathBuf> {
    if let Some(dir) = option_env!("CARGO_TARGET_DIR").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    option_env!("CARGO_MANIFEST_DIR").map(|m| PathBuf::from(m).join("target"))
}

// ---------------------------------------------------------------------------
// Acceptance candidate resolution (phase-04 task-18). The recorded jgrapht
// baseline's provenance is the OPTIMIZED release binary, so acceptance measures
// `target/release/apg` — never the debug sibling. A debug measurement can never
// be mistaken for a passing acceptance: the ONLY observable is the loud failure
// naming `cargo build --release`.
// ---------------------------------------------------------------------------

/// The cargo profile an acceptance candidate was built under, plus its absolute
/// binary path — the provenance every acceptance artifact records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptanceProfile {
    label: &'static str,
    binary_path: PathBuf,
}

impl AcceptanceProfile {
    /// Classifies an existing candidate path: the label is DERIVED from the
    /// artifact (`target/<profile>/apg`), never hard-coded.
    pub fn from_path(path: &Path) -> AcceptanceProfile {
        AcceptanceProfile {
            label: classify_candidate_profile(path),
            binary_path: path.to_path_buf(),
        }
    }

    /// The profile label: `release` or `debug`.
    pub fn label(&self) -> &'static str {
        self.label
    }

    /// The absolute candidate binary path (for the acceptance artifacts).
    pub fn binary_path(&self) -> &Path {
        &self.binary_path
    }

    pub fn is_release(&self) -> bool {
        self.label == "release"
    }

    /// An acceptance artifact is satisfied ONLY by a release-profile candidate;
    /// a debug measurement is NOT acceptance-satisfied.
    pub fn is_acceptance_satisfied(&self) -> bool {
        self.is_release()
    }
}

/// The profile label a candidate artifact path belongs to — the component after
/// `target` (`release`/`debug`). Derived from the path, never hard-coded; a path
/// with no `target/<profile>` component classifies as `debug` (never a silent
/// release).
pub fn classify_candidate_profile(path: &Path) -> &'static str {
    let mut comps = path.components();
    while let Some(c) = comps.next() {
        if c.as_os_str().to_str() == Some("target") {
            return match comps.next().and_then(|p| p.as_os_str().to_str()) {
                Some("release") => "release",
                _ => "debug",
            };
        }
    }
    "debug"
}

/// Resolves `target/release/apg` under `target_dir` — the acceptance candidate.
/// `None` when that artifact is absent: the caller MUST fail loudly naming
/// `cargo build --release`; it NEVER falls back to the debug sibling.
pub fn resolve_acceptance_candidate(target_dir: &Path) -> Option<AcceptanceProfile> {
    let candidate = target_dir.join("release").join(apg_bin_name());
    if candidate.is_file() {
        Some(AcceptanceProfile::from_path(&candidate))
    } else {
        None
    }
}

/// [`resolve_acceptance_candidate`] against an explicit `target/` dir, failing
/// LOUDLY naming `cargo build --release` when the release artifact is absent —
/// never a silent debug fallback. Split out so the single observable is testable
/// against a synthetic target tree.
pub fn resolve_acceptance_candidate_or_panic(target_dir: &Path) -> AcceptanceProfile {
    resolve_acceptance_candidate(target_dir).unwrap_or_else(|| {
        panic!(
            "acceptance candidate `target/release/apg` is absent under {} — run \
             `cargo build --release` first (a debug-profile measurement is NOT \
             acceptance-satisfied)",
            target_dir.display()
        )
    })
}

/// The acceptance candidate: `target/release/apg`, the artifact
/// `cargo build --release` leaves. Panics naming `cargo build --release` when it
/// is absent — a debug-profile run can never resolve a candidate, so it can
/// never be acceptance-satisfied (exactly ONE observable: this loud failure).
pub fn apg_bin_profile() -> AcceptanceProfile {
    let dir = acceptance_target_dir().unwrap_or_else(|| {
        panic!("cannot locate the cargo target directory — run `cargo build --release` first")
    });
    resolve_acceptance_candidate_or_panic(&dir)
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

    /// A configured real-CLI `apg` invocation against an EXPLICIT binary — the
    /// acceptance harness's release-profile candidate ([`apg_bin_profile`]) —
    /// rather than the running profile's [`apg_bin`].
    pub fn with_bin(bin: PathBuf, args: &[&str]) -> ApgCommand {
        ApgCommand {
            bin,
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
/// `git::commit_files` uses).
pub fn wt_commit(wt: &Path, rels: &[&str], msg: &str) {
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
        .unwrap();
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
        crate::run_pipeline(records, &[], &[], "go", None, None, &mut log);
        Ok(())
    };
    std::env::set_current_dir(old)?;
    result
}

// ---------------------------------------------------------------------------
// Acceptance harness (phase-04 task-5). The SINGLE acceptance workload is
// jgrapht. Resolution is explicit-source-first, staging is a READ-ONLY copy of
// the real checkout's HEAD into a fresh scratch /tmp git repo, and only the
// CANDIDATE binary is ever pointed at the staged copy
// (global.constraint.no-real-project-test). A missing checkout is a LOUD
// failure by default; the only skip is the explicit `APG_ACCEPTANCE_SKIP=1`
// opt-in, which records a machine-readable SKIP disposition that is NOT
// acceptance-satisfied.
// ---------------------------------------------------------------------------

/// The single acceptance workload (the jgrapht reference project). There is no
/// netbeans workload.
pub const ACCEPTANCE_WORKLOAD: &str = "jgrapht";
/// Per-workload override: the jgrapht checkout path.
pub const ACCEPTANCE_JGRAPHT_ENV: &str = "APG_ACCEPTANCE_JGRAPHT";
/// Shared-root override: jgrapht is `<root>/jgrapht`.
pub const ACCEPTANCE_ROOT_ENV: &str = "APG_ACCEPTANCE_ROOT";
/// The documented opt-in skip (`1`/`true`); a skipped run is NOT
/// acceptance-satisfied.
pub const ACCEPTANCE_SKIP_ENV: &str = "APG_ACCEPTANCE_SKIP";

/// Which source the jgrapht reference checkout was resolved from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcceptanceSource {
    /// `APG_ACCEPTANCE_JGRAPHT` (the per-workload override).
    EnvJgrapht,
    /// `APG_ACCEPTANCE_ROOT` (jgrapht is `<root>/jgrapht`).
    EnvRoot,
    /// The conventional `~/jgrapht`.
    ConventionalHome,
}

impl AcceptanceSource {
    /// The label recorded in the machine-readable artifact.
    pub fn label(&self) -> &'static str {
        match self {
            AcceptanceSource::EnvJgrapht => ACCEPTANCE_JGRAPHT_ENV,
            AcceptanceSource::EnvRoot => ACCEPTANCE_ROOT_ENV,
            AcceptanceSource::ConventionalHome => "~/jgrapht",
        }
    }
}

/// Why an acceptance harness could not be built.
#[derive(Debug)]
pub enum AcceptanceError {
    /// No checkout at the resolved path and no opt-in skip — a LOUD failure.
    Missing {
        workload: &'static str,
        source: AcceptanceSource,
        path: PathBuf,
        reason: String,
    },
    /// The explicit opt-in skip was taken: a SKIP disposition was recorded and
    /// the run is NOT acceptance-satisfied.
    Skipped {
        workload: &'static str,
        source: AcceptanceSource,
        path: PathBuf,
        reason: String,
        disposition: PathBuf,
    },
}

impl AcceptanceError {
    /// True for the explicit opt-in skip (the caller may return early; the
    /// recorded disposition marks the run unsatisfied).
    pub fn is_skip(&self) -> bool {
        matches!(self, AcceptanceError::Skipped { .. })
    }
}

impl std::fmt::Display for AcceptanceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AcceptanceError::Missing {
                workload,
                source,
                path,
                reason,
            } => write!(
                f,
                "acceptance workload `{workload}` is MISSING: {reason} (at {}, \
                 resolved from {src}). Set {j} to the {workload} checkout, or {r} to a \
                 root containing {workload}/, or provide ~/{workload}.",
                path.display(),
                src = source.label(),
                j = ACCEPTANCE_JGRAPHT_ENV,
                r = ACCEPTANCE_ROOT_ENV
            ),
            AcceptanceError::Skipped {
                workload,
                source,
                path,
                reason,
                disposition,
            } => write!(
                f,
                "acceptance workload `{workload}` SKIPPED by {skip}=1: {reason} (at {}, \
                 resolved from {src}). This run is NOT acceptance-satisfied — the task \
                 stays open and the skip must be filed as feedback. SKIP disposition: {}",
                path.display(),
                disposition.display(),
                skip = ACCEPTANCE_SKIP_ENV,
                src = source.label(),
            ),
        }
    }
}

/// Resolution precedence: `APG_ACCEPTANCE_JGRAPHT`, then
/// `APG_ACCEPTANCE_ROOT/jgrapht`, then `~/jgrapht`. Returns the path even when
/// it is missing so the caller can decide missing vs. skip.
pub fn resolve_jgrapht() -> (AcceptanceSource, PathBuf) {
    if let Some(p) = std::env::var_os(ACCEPTANCE_JGRAPHT_ENV).filter(|v| !v.is_empty()) {
        return (AcceptanceSource::EnvJgrapht, PathBuf::from(p));
    }
    if let Some(root) = std::env::var_os(ACCEPTANCE_ROOT_ENV).filter(|v| !v.is_empty()) {
        return (
            AcceptanceSource::EnvRoot,
            PathBuf::from(root).join(ACCEPTANCE_WORKLOAD),
        );
    }
    let path = match std::env::var_os("HOME") {
        Some(h) if !h.is_empty() => PathBuf::from(h).join(ACCEPTANCE_WORKLOAD),
        _ => PathBuf::from(ACCEPTANCE_WORKLOAD),
    };
    (AcceptanceSource::ConventionalHome, path)
}

fn acceptance_skip_requested() -> bool {
    matches!(
        std::env::var(ACCEPTANCE_SKIP_ENV).as_deref(),
        Ok("1") | Ok("true") | Ok("yes")
    )
}

/// The staged acceptance workload: a fresh scratch `/tmp` git repo holding the
/// reference checkout's HEAD content, an isolated `HOME`, and an isolated
/// `APG_FRONTEND_DIR` carrying only the Java frontend (the workload is Java, so
/// detection can never accidentally run every installed frontend on it).
pub struct AcceptanceHarness {
    pub workload: &'static str,
    pub source: AcceptanceSource,
    pub source_path: PathBuf,
    /// The RELEASE-profile candidate the scenarios measure (phase-04 task-18):
    /// resolution panics naming `cargo build --release` when it is absent, so a
    /// debug-profile run can never be acceptance-satisfied.
    pub candidate: AcceptanceProfile,
    /// The scratch base (teardown removes it).
    pub base: PathBuf,
    /// The staged read-only copy (a fresh git repo checked out at the source's
    /// HEAD).
    pub repo: PathBuf,
    /// Isolated `HOME` for `apg init`'s suite install.
    pub home: PathBuf,
    /// Isolated `APG_FRONTEND_DIR` holding only `java-classes`.
    pub frontend_dir: PathBuf,
    /// How many files the read-only staging wrote.
    pub staged_files: usize,
}

impl AcceptanceHarness {
    /// Build the jgrapht acceptance harness, or fail loudly / record a skip.
    pub fn jgrapht() -> Result<AcceptanceHarness, AcceptanceError> {
        let (source, source_path) = resolve_jgrapht();
        if !source_path.is_dir() {
            let reason = format!("no checkout directory at {}", source_path.display());
            if acceptance_skip_requested() {
                let disposition =
                    write_skip_disposition(ACCEPTANCE_WORKLOAD, &source, &source_path, &reason);
                return Err(AcceptanceError::Skipped {
                    workload: ACCEPTANCE_WORKLOAD,
                    source,
                    path: source_path,
                    reason,
                    disposition,
                });
            }
            return Err(AcceptanceError::Missing {
                workload: ACCEPTANCE_WORKLOAD,
                source,
                path: source_path,
                reason,
            });
        }

        let base = std::env::temp_dir().join(format!(
            "apg-acceptance-{}-{}",
            ACCEPTANCE_WORKLOAD,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("staged");
        let home = base.join("home");
        std::fs::create_dir_all(&home).unwrap();
        // Keep `apg init` hermetic/fast: pre-create the opencode plugin dir so
        // it never shells out to npm.
        std::fs::create_dir_all(home.join(".opencode/node_modules/@opencode-ai/plugin")).unwrap();

        // The staged copy carries ONLY the Java frontend, so auto-detection on
        // the Java workload yields exactly `java` (never a multi-frontend run).
        let frontend_dir = base.join("frontends");
        let src_frontend = apg_bin()
            .parent()
            .expect("apg binary parent")
            .join("frontends")
            .join("java-classes");
        assert!(
            src_frontend.is_dir(),
            "the staged Java frontend must exist at {} — run `cargo build` first",
            src_frontend.display()
        );
        copy_dir(&src_frontend, &frontend_dir.join("java-classes"))
            .unwrap_or_else(|e| panic!("stage the Java frontend: {e:#}"));

        let staged_files = stage_checkout_readonly(&source_path, &repo).unwrap_or_else(|e| {
            panic!(
                "stage {} read-only from {}: {e:#}",
                ACCEPTANCE_WORKLOAD,
                source_path.display()
            )
        });
        commit_all_files(&repo, "staged acceptance checkout");

        // Phase-04 task-18: the measured candidate is the RELEASE artifact. The
        // frontend staging above stays on the running profile's `apg_bin` (the
        // Java frontend is language tooling, not the measured scanner and is
        // always built under `cargo build`/`cargo test`).
        let candidate = apg_bin_profile();

        Ok(AcceptanceHarness {
            workload: ACCEPTANCE_WORKLOAD,
            source,
            source_path,
            candidate,
            base,
            repo,
            home,
            frontend_dir,
            staged_files,
        })
    }

    /// Runs the CANDIDATE binary against the staged repo root.
    pub fn run(&self, args: &[&str]) -> Output {
        self.run_in(&self.repo, args)
    }

    /// Runs the CANDIDATE binary with cwd `dir` (the staged repo, or one of its
    /// worktrees). The candidate is the RELEASE-profile artifact resolved at
    /// harness construction (phase-04 task-18), never the debug sibling.
    pub fn run_in(&self, dir: &Path, args: &[&str]) -> Output {
        ApgCommand::with_bin(self.candidate.binary_path().to_path_buf(), args)
            .cwd(dir)
            .env("HOME", &self.home.to_string_lossy())
            .env("APG_FRONTEND_DIR", &self.frontend_dir.to_string_lossy())
            .output()
    }

    /// Removes the scratch copy (and its staged frontends / HOME).
    pub fn discard(&self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
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

/// The `git archive HEAD` equivalent: writes every blob of the source's HEAD
/// tree into the fresh scratch repo `dest`, git2-only and READ-ONLY on `src`
/// (the real checkout is never written to or git-mutated). Submodule gitlinks
/// and any non-blob entry are skipped. Returns the number of files written.
fn stage_checkout_readonly(src: &Path, dest: &Path) -> anyhow::Result<usize> {
    let src_repo = git2::Repository::open(src)
        .map_err(|e| anyhow::anyhow!("open reference checkout {}: {e}", src.display()))?;
    let head = src_repo.head()?.peel_to_commit()?;
    let tree = head.tree()?;
    let mut opts = git2::RepositoryInitOptions::new();
    opts.initial_head("refs/heads/main");
    let _dest_repo = git2::Repository::init_opts(dest, &opts)?;
    write_tree_blobs(&src_repo, &tree, Path::new(""), dest)
}

fn write_tree_blobs(
    src_repo: &git2::Repository,
    tree: &git2::Tree,
    prefix: &Path,
    dest: &Path,
) -> anyhow::Result<usize> {
    let mut written = 0usize;
    for entry in tree.iter() {
        let name = entry
            .name()
            .ok_or_else(|| anyhow::anyhow!("non-UTF-8 tree entry"))?;
        let rel = prefix.join(name);
        match entry.kind() {
            Some(git2::ObjectType::Blob) => {
                let blob = src_repo.find_blob(entry.id())?;
                let out = dest.join(&rel);
                if let Some(parent) = out.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&out, blob.content())?;
                written += 1;
            }
            Some(git2::ObjectType::Tree) => {
                let sub = src_repo.find_tree(entry.id())?;
                written += write_tree_blobs(src_repo, &sub, &rel, dest)?;
            }
            // Submodule gitlinks and anything else are not source files.
            _ => {}
        }
    }
    Ok(written)
}

/// Stages and commits every change under `dir` (git2 — the git CLI is never
/// shelled out to anywhere in src), tolerating an unborn HEAD.
pub fn commit_all_files(dir: &Path, msg: &str) {
    let repo = git2::Repository::open(dir).unwrap();
    let mut cfg = repo.config().unwrap();
    cfg.set_str("user.name", "apg acceptance").unwrap();
    cfg.set_str("user.email", "apg-acceptance@example.com")
        .unwrap();
    drop(cfg);
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
    repo.commit(Some("HEAD"), &sig, &sig, msg, &tree, parents.as_slice())
        .unwrap();
}

/// The gitignored acceptance-artifact directory (`apg/.trans/acceptance/`).
pub fn acceptance_artifact_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(specs::LAYOUT)
        .join(specs::TRANS)
        .join("acceptance")
}

/// Writes a machine-readable acceptance artifact, returning its path. Also
/// echoes the one-line JSON to stderr, so the measured numbers are visible
/// under `--nocapture` (the artifact itself lives in the read-guarded
/// `apg/.trans/`).
pub fn write_acceptance_artifact(name: &str, value: &serde_json::Value) -> PathBuf {
    let dir = acceptance_artifact_dir();
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(
        &path,
        format!("{}\n", serde_json::to_string_pretty(value).unwrap()),
    )
    .unwrap();
    eprintln!(
        "acceptance artifact {}: {}",
        path.display(),
        serde_json::to_string(value).unwrap()
    );
    path
}

/// Records a machine-readable SKIP disposition (workload, source, reason,
/// timestamp) — NOT acceptance-satisfied.
pub fn write_skip_disposition(
    workload: &str,
    source: &AcceptanceSource,
    path: &Path,
    reason: &str,
) -> PathBuf {
    let value = serde_json::json!({
        "acceptance": "SKIP",
        "workload": workload,
        "source": source.label(),
        "resolved_path": path.display().to_string(),
        "reason": reason,
        "recorded_at": crate::git::now_iso8601(),
        "acceptance_satisfied": false,
    });
    write_acceptance_artifact(&format!("skip-{workload}.json"), &value)
}

// ---------------------------------------------------------------------------
// Enumerated equivalence-oracle readers (phase-02 task-17 / phase-03 task-9),
// shared by the phase-04 acceptance scenarios. Non-#[test] harness helpers, so
// they live at module level.
// ---------------------------------------------------------------------------

/// Opens a DB file read-only, runs `f`, and closes it — one `Database::new`
/// per oracle pass, so a large acceptance DB is not reopened per query.
pub fn with_db<T>(path: &Path, f: impl FnOnce(&lbug::Connection) -> T) -> T {
    let db = lbug::Database::new(path, lbug::SystemConfig::default().read_only(true))
        .unwrap_or_else(|e| panic!("open {}: {e}", path.display()));
    let conn = lbug::Connection::new(&db).unwrap();
    let out = f(&conn);
    drop(conn);
    drop(db);
    out
}

/// Runs `query` on an open read-only connection, returning every row's cells as
/// strings.
pub fn query_rows(conn: &lbug::Connection, query: &str) -> Vec<Vec<String>> {
    conn.query(query)
        .map(|r| {
            r.map(|row| row.iter().map(|v| v.to_string()).collect::<Vec<String>>())
                .collect::<Vec<Vec<String>>>()
        })
        .unwrap_or_default()
}

/// Runs `query` against a DB file opened read-only, returning every row's cells
/// as strings.
pub fn db_rows(path: &Path, query: &str) -> Vec<Vec<String>> {
    with_db(path, |conn| query_rows(conn, query))
}

/// Every table's row count in a DB file opened read-only — the per-label NODE
/// counts AND the per-rel-type COUNTS in one map (`show_tables()` enumerates
/// both node and REL tables). The DB is opened once for the whole pass.
pub fn db_table_counts(path: &Path) -> BTreeMap<String, i64> {
    with_db(path, |conn| {
        let mut out = BTreeMap::new();
        let tables = query_rows(conn, "CALL show_tables() RETURN name, type");
        for row in tables {
            let table = row.first().cloned().unwrap_or_default();
            let kind = row.get(1).cloned().unwrap_or_default();
            let q = if kind == "REL" {
                format!("MATCH ()-[r:{table}]->() RETURN count(*)")
            } else {
                format!("MATCH (n:{table}) RETURN count(*)")
            };
            let n = query_rows(conn, &q)
                .first()
                .and_then(|r| r.first())
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(-1);
            out.insert(table, n);
        }
        out
    })
}

/// The `UnresolvedTarget` rows of a DB file as `(fqn, category)`.
pub fn db_unresolved_rows(path: &Path) -> BTreeSet<(String, String)> {
    db_rows(path, "MATCH (n:UnresolvedTarget) RETURN n.fqn, n.category")
        .into_iter()
        .map(|r| {
            (
                r.first().cloned().unwrap_or_default(),
                r.get(1).cloned().unwrap_or_default(),
            )
        })
        .collect()
}

/// The single `Scan` row of a DB file as `(sha, clean, key, at)`.
pub fn db_scan_row(path: &Path) -> (String, String, String, String) {
    let rows = db_rows(
        path,
        "MATCH (s:Scan) RETURN s.git_sha, s.git_clean, s.content_key, s.scanned_at",
    );
    assert_eq!(rows.len(), 1, "exactly one Scan row in {}", path.display());
    let r = &rows[0];
    (
        r.first().cloned().unwrap_or_default(),
        r.get(1).cloned().unwrap_or_default(),
        r.get(2).cloned().unwrap_or_default(),
        r.get(3).cloned().unwrap_or_default(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layers::{self, Layer};

    /// e2e tier -- real I/O: both tests spawn the candidate `apg` binary,
    /// create scratch repos and inspect the socket/`db.lbug` on disk. Each is
    /// `#[ignore]`d, so a plain `cargo test` never runs one; the only entry
    /// point is the named guard `cargo test-e2e`
    /// (= `cargo test tests::e2e:: -- --ignored`). apg.testutil's own
    /// non-`#[test]` items (Repo, ApgCommand, spawn_apg, start_session_process,
    /// scan_checkout, payload helpers) are the E2E HARNESS and live at module
    /// level, never inside a tier. PIN: apg.testutil is e2e-only
    /// infrastructure — it must not be used by a unit/int test.
    mod e2e {
        use super::*;

        /// Phase-03 task-19: a SIGKILLed session leaves NO half-written durable
        /// state and no process holding `db.lbug`, and the stale socket it leaves
        /// behind (no live process) is reclaimed by the next `apg session start`.
        #[test]
        #[ignore = "e2e tier: real I/O (spawned apg/scratch repo/db.lbug); run via cargo test-e2e"]
        fn killed_session_loses_nothing_and_its_stale_socket_is_reclaimed() {
            let (repo, wt, wt_apg) = project_with_db("session-crash");
            let home = repo.root.join("home");
            let session = start_session_process(&wt, &home);

            // One routed mutation so there is durable state to inspect.
            let add = ApgCommand::new(&["node", "add", "requirements", "requirement", "survivor"])
                .cwd(&wt)
                .env("HOME", home.to_str().unwrap())
                .output();
            assert!(
                add.status.success(),
                "{}",
                String::from_utf8_lossy(&add.stderr)
            );

            // SIGKILL — deliberately NOT a graceful `end`.
            let pid = session.child.id() as i32;
            unsafe { libc::kill(pid, libc::SIGKILL) };
            let out = session.child.wait_with_output().unwrap();
            assert!(
                !out.status.success(),
                "the session was killed, not ended cleanly"
            );

            // (a) no half-written node file: the survivor parses.
            let nf =
                layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "survivor")
                    .unwrap();
            assert_eq!(nf.name, "survivor");
            // (b) no paired edge half mismatched (the store still pairs cleanly).
            layers::check_edge_pairing(&layers::read_existing_nodes(&wt_apg).unwrap()).unwrap();
            // (c) no process holds db.lbug: a direct read-write open succeeds now.
            let db = crate::artifacts::ArtifactDb::open(&wt_apg).unwrap();
            assert!(db.has_node("requirements.requirement.survivor"));
            drop(db);

            // (d) the SIGKILL left the socket file behind; the next start reclaims
            // it (no live process behind it) and serves normally.
            let socket = crate::session::socket_path(&wt_apg);
            assert!(socket.exists(), "SIGKILL leaves the stale socket behind");
            let session2 = start_session_process(&wt, &home);
            let end = spawn_apg(&["session", "end"], &wt);
            assert!(
                end.status.success(),
                "{}",
                String::from_utf8_lossy(&end.stderr)
            );
            let out2 = session2.child.wait_with_output().unwrap();
            assert!(
                out2.status.success(),
                "{}",
                String::from_utf8_lossy(&out2.stderr)
            );
            let stderr2 = String::from_utf8_lossy(&out2.stderr);
            assert!(
                stderr2.contains("reclaimed stale socket"),
                "the next start must reclaim the stale socket: {stderr2}"
            );

            remove(&repo);
        }

        /// Phase-04 task-2 (acceptance): a REAL long-running `apg session start`,
        /// SIGKILLed (NOT gracefully ended) mid-life, loses nothing durable and
        /// leaves no partial store:
        ///
        /// (a) no node file left half-written — every expected file parses with the
        ///     right identity;
        /// (b) no paired edge half mismatched — the store still pairs and both the
        ///     source out-half and target in-half of the routed edge are present;
        /// (c) no process holds `db.lbug` — a direct read-write open succeeds;
        /// (d) the stale socket is reclaimed by the next `apg session start` with
        ///     no live process behind it.
        ///
        /// The routed mutations deliberately write a node AND an edge (both
        /// endpoint files), so a crash between the two halves of the edge would be
        /// caught by (b) — the node-only phase-03 regression cannot see that.
        #[test]
        #[ignore = "e2e tier: real I/O (spawned apg/scratch repo/db.lbug); run via cargo test-e2e"]
        fn acceptance_crash_durability_no_partial_files_no_db_holder_and_socket_reclaim() {
            let (repo, wt, wt_apg) = project_with_db("accept-crash");
            let home = repo.root.join("home");
            let session = start_session_process(&wt, &home);

            // Two routed node adds and the routed edge between them.
            for name in ["crash-a", "crash-b"] {
                let add = ApgCommand::new(&["node", "add", "requirements", "requirement", name])
                    .cwd(&wt)
                    .env("HOME", home.to_str().unwrap())
                    .output();
                assert!(
                    add.status.success(),
                    "{name}: {}",
                    String::from_utf8_lossy(&add.stderr)
                );
            }
            let edge = ApgCommand::new(&[
                "edge",
                "add",
                "depends-on",
                "requirements.requirement.crash-a",
                "requirements.requirement.crash-b",
            ])
            .cwd(&wt)
            .env("HOME", home.to_str().unwrap())
            .output();
            assert!(
                edge.status.success(),
                "{}",
                String::from_utf8_lossy(&edge.stderr)
            );

            // SIGKILL — deliberately NOT a graceful `end`.
            let pid = session.child.id() as i32;
            unsafe { libc::kill(pid, libc::SIGKILL) };
            let out = session.child.wait_with_output().unwrap();
            assert!(
                !out.status.success(),
                "the session was killed, not ended cleanly"
            );

            // (a) no half-written node file: every expected file parses with its
            // identity intact.
            for name in ["crash-a", "crash-b"] {
                let nf = layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", name)
                    .unwrap();
                assert_eq!(nf.name, name, "node file {name} must be complete");
            }

            // (b) no paired edge half mismatched: the store pairs cleanly AND both
            // halves of the routed edge are present.
            let all = layers::read_existing_nodes(&wt_apg).unwrap();
            layers::check_edge_pairing(&all).unwrap();
            let a = layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "crash-a")
                .unwrap();
            assert!(
                a.out
                    .iter()
                    .any(|oe| oe.kind == "depends-on"
                        && oe.target == "requirements.requirement.crash-b"),
                "the source out-half must be present and complete"
            );
            let b = layers::read_node_file(&wt_apg, Layer::Requirements, "requirement", "crash-b")
                .unwrap();
            assert!(
                b.in_edges
                    .iter()
                    .any(|ie| ie.kind == "depends-on"
                        && ie.source == "requirements.requirement.crash-a"),
                "the target in-half must be present and match the out-half"
            );

            // (c) no process holds db.lbug: a direct read-write open succeeds now
            // (the SIGKILL released the OS lock).
            let db = crate::artifacts::ArtifactDb::open(&wt_apg).unwrap();
            assert!(db.has_node("requirements.requirement.crash-a"));
            assert!(db.has_node("requirements.requirement.crash-b"));
            drop(db);

            // (d) the SIGKILL left the socket file behind; the next start reclaims
            // it (no live process behind it) and serves normally.
            let socket = crate::session::socket_path(&wt_apg);
            assert!(socket.exists(), "SIGKILL leaves the stale socket behind");
            let session2 = start_session_process(&wt, &home);
            let end = spawn_apg(&["session", "end"], &wt);
            assert!(
                end.status.success(),
                "{}",
                String::from_utf8_lossy(&end.stderr)
            );
            let out2 = session2.child.wait_with_output().unwrap();
            assert!(
                out2.status.success(),
                "{}",
                String::from_utf8_lossy(&out2.stderr)
            );
            let stderr2 = String::from_utf8_lossy(&out2.stderr);
            assert!(
                stderr2.contains("reclaimed stale socket"),
                "the next start must reclaim the stale socket: {stderr2}"
            );

            remove(&repo);
        }

        /// Phase-04 task-26: the acceptance harness resolves the RELEASE-profile
        /// candidate and never silently accepts a debug one. Real `target/` tree
        /// access ⇒ e2e by global.constraint.test-tier-boundaries.
        #[test]
        #[ignore = "e2e tier: real I/O (temp dir/target tree); run via cargo test-e2e"]
        fn acceptance_candidate_resolves_release_and_rejects_debug() {
            fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
                payload
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
                    .unwrap_or_default()
            }

            // (a) profile classification is DERIVED from the artifact path.
            assert_eq!(
                classify_candidate_profile(Path::new("/x/target/release/apg")),
                "release"
            );
            assert_eq!(
                classify_candidate_profile(Path::new("/x/target/debug/apg")),
                "debug"
            );
            assert_eq!(
                classify_candidate_profile(Path::new("relative/apg")),
                "debug"
            );

            // (d) the profile label + absolute path are queryable, and a debug
            // result is NOT acceptance-satisfied.
            let release = AcceptanceProfile::from_path(Path::new("/x/target/release/apg"));
            assert_eq!(release.label(), "release");
            assert_eq!(release.binary_path(), Path::new("/x/target/release/apg"));
            assert!(release.is_release() && release.is_acceptance_satisfied());
            let debug = AcceptanceProfile::from_path(Path::new("/x/target/debug/apg"));
            assert_eq!(debug.label(), "debug");
            assert!(!debug.is_release());
            assert!(!debug.is_acceptance_satisfied());

            // (b)/(c) synthetic target trees: release present ⇒ THAT path (never the
            // debug sibling); release absent ⇒ resolution yields no candidate.
            let dir = std::env::temp_dir()
                .join(format!("apg-accept-profile-{}", std::process::id()))
                .join("target");
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.join("release")).unwrap();
            std::fs::create_dir_all(dir.join("debug")).unwrap();
            std::fs::write(dir.join("release").join(apg_bin_name()), b"release").unwrap();
            std::fs::write(dir.join("debug").join(apg_bin_name()), b"debug").unwrap();

            let resolved = resolve_acceptance_candidate(&dir).expect("release candidate present");
            assert_eq!(resolved.label(), "release");
            assert!(
                resolved
                    .binary_path()
                    .ends_with(Path::new("release").join(apg_bin_name())),
                "the release artifact must be returned: {}",
                resolved.binary_path().display()
            );
            assert!(
                !resolved.binary_path().to_string_lossy().contains("/debug/"),
                "the debug sibling must never be returned"
            );

            std::fs::remove_file(dir.join("release").join(apg_bin_name())).unwrap();
            assert!(
                resolve_acceptance_candidate(&dir).is_none(),
                "an absent release artifact must not fall back to the debug sibling"
            );

            // (c) the acceptance entry point fails LOUDLY naming `cargo build
            // --release` — never a silent debug fallback (the synthetic tree has
            // only a debug sibling, so the fallback is exactly what must not
            // happen).
            let prev = std::panic::take_hook();
            std::panic::set_hook(Box::new(|_| {}));
            let result = std::panic::catch_unwind(|| resolve_acceptance_candidate_or_panic(&dir));
            std::panic::set_hook(prev);
            let msg = panic_message(result.expect_err("absent release must panic"));
            assert!(
                msg.contains("cargo build --release"),
                "the loud failure must name `cargo build --release`: {msg}"
            );
            assert!(
                !msg.contains("/debug/"),
                "the failure must never fall back to the debug sibling: {msg}"
            );

            // When this checkout itself has the release artifact, the entry point
            // resolves THAT path as the release candidate.
            let real = acceptance_target_dir()
                .unwrap()
                .join("release")
                .join(apg_bin_name());
            if real.is_file() {
                let p = apg_bin_profile();
                assert_eq!(p.label(), "release");
                assert_eq!(p.binary_path(), real.as_path());
                assert!(p.is_acceptance_satisfied());
            }

            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}
