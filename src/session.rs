//! Session-scoped single-writer coordinator (phase-03).
//!
//! `apg session start` launches a process that owns the worktree's
//! `apg/.trans/db.lbug` **exclusively** and serves routed mutations and reads
//! over a Unix socket at `apg/.trans/session.sock`. While it is live the
//! coordinator is the single writer of BOTH the DB and the durable mutation: it
//! performs the node-file read-modify-write, the one-commit-per-mutation git
//! commit, and the projection apply itself, in receive order. It holds the SAME
//! extended `specs.lock` flock the direct path uses for the session's life, so a
//! non-routing/direct `apg node` / `apg edge` writer cannot RMW the same node
//! files concurrently and lose an edge.
//!
//! Routing is transparent: [`crate::node_cmd::cmd_node`] /
//! [`crate::node_cmd::cmd_edge`] detect the live socket and forward; with no
//! session they take the serialized direct path. Reads route through the same
//! socket ([`crate::cmd_query`]) so a separate reader sees the post-mutation
//! state immediately, with no lock error and no waiting for the session to end.
//!
//! Amortization is of the DB **open**, never of visibility: the DB is opened
//! once for the session's life ([`Coordinator`]'s owned handle) and every routed
//! durable mutation is admitted into an in-memory write-back buffer of
//! node-file writes/deletes whose exact projection delta is applied
//! synchronously through that handle AT ADMISSION — `apg/layers/**` and git
//! stay at the last saved state until [`Coordinator::save`], so a routed read
//! observes the buffered intention while a direct read observes the last saved
//! state. Forwarded mutations carry a client-generated id; the coordinator
//! records applied ids so a replay is at-most-once, and a failed forward is
//! reported — the client never silently falls back to the direct path
//! mid-flight.

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::artifacts::{ArtifactDb, SpecLockGuard, acquire_spec_lock};
use crate::layers::{self, Layer, NodeFile};
use crate::specs;

/// The session socket file name under `apg/.trans/`.
pub const SOCKET_NAME: &str = "session.sock";

/// The observable per-open marker the session prints to stderr; the
/// amortization test counts it (phase-03 task-16: an N-mutation session must
/// open the DB materially fewer than N times).
pub const DB_OPEN_MARKER: &str = "apg session: db-open";

/// Per-process sequence mixed into every client id so a forwarded mutation id
/// is unique even within one process.
static CLIENT_SEQ: AtomicU64 = AtomicU64::new(0);

/// The live-session socket for a layout root.
///
/// Preferred location is `<apg_root>/.trans/session.sock` (the socket lives
/// beside the DB it guards). Unix-domain socket paths are capped by the
/// platform's `SUN_LEN` (≈104 bytes on macOS), which a deeply nested worktree
/// path — including the test harness's per-user temp dir — exceeds. When the
/// preferred path does not fit, fall back to a deterministic short path keyed
/// by the canonical layout root, so both the server and every client resolve
/// the same socket without any shared state.
pub fn socket_path(apg_root: &Path) -> PathBuf {
    let preferred = apg_root.join(specs::TRANS).join(SOCKET_NAME);
    if fits_sun_len(&preferred) {
        return preferred;
    }
    short_socket_path(apg_root)
}

/// Conservative `SUN_LEN` bound (the worst case is 104 bytes including the
/// trailing NUL, so anything under 100 is safe on every target).
fn fits_sun_len(path: &Path) -> bool {
    path.as_os_str().len() < 100
}

/// A short, deterministic socket path for a layout root: `<tmp>/apg-session-<hash>.sock`
/// where `<hash>` is the canonical layout-root path. `/tmp` is used directly
/// (not `$TMPDIR`) so the path is short and identical for every process
/// regardless of environment.
fn short_socket_path(apg_root: &Path) -> PathBuf {
    let canonical = std::fs::canonicalize(apg_root).unwrap_or_else(|_| apg_root.to_path_buf());
    let mut hasher = DefaultHasher::new();
    canonical.hash(&mut hasher);
    let hash = hasher.finish();
    PathBuf::from(format!("/tmp/apg-session-{hash:016x}.sock"))
}

/// The layout root of the checkout the process runs in (walk-up discovery).
pub fn require_apg_root() -> anyhow::Result<PathBuf> {
    let start = std::env::current_dir()?;
    specs::find_apg_root(&start)
        .ok_or_else(|| anyhow::anyhow!("no apg/ directory found from {}", start.display()))
}

// ---------------------------------------------------------------------------
// Wire protocol: one JSON line per message over the Unix socket.
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum Request {
    /// Liveness probe (session detection).
    Ping,
    /// Server-side graceful shutdown — the long-running `start` process releases
    /// the DB/socket and `serve` exits. The `apg session end` client only sends
    /// this; it never releases a DB/socket it does not own.
    End,
    /// A routed durable mutation: the `node`/`edge` subcommand args plus the
    /// at-most-once client id.
    Mutate {
        client_id: String,
        kind: String,
        args: Vec<String>,
    },
    /// A routed read: the Cypher text (already `;`-terminated) plus the output
    /// format. Served against the session-held `db.lbug`.
    Query { query: String, json: bool },
    /// The client asks the coordinator to make the whole buffered set durable:
    /// flush the buffered node-file writes/deletes, make exactly one commit,
    /// re-anchor `scan_meta`, then clear the buffer — the protocol counterpart
    /// of [`Coordinator::save`].
    Save,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) enum Reply {
    Pong,
    Ok { output: String },
    Err { message: String },
}

fn write_msg<T: Serialize>(stream: &mut UnixStream, msg: &T) -> std::io::Result<()> {
    let line = serde_json::to_string(msg).expect("serialize session message");
    stream.write_all(line.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()
}

fn read_msg<T: for<'de> Deserialize<'de>>(stream: &UnixStream) -> std::io::Result<T> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "session peer closed without a message",
        ));
    }
    serde_json::from_str(line.trim_end())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// A fresh, unique client id for a forwarded mutation: pid + monotonic clock +
/// a per-process sequence.
fn new_client_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = CLIENT_SEQ.fetch_add(1, Ordering::Relaxed);
    format!("{}-{nanos}-{seq}", std::process::id())
}

// ---------------------------------------------------------------------------
// Detection + forwarding (client side)
// ---------------------------------------------------------------------------

/// True when a live session is served at `socket` — the socket exists AND a
/// connect + `Ping` round-trips (a stale socket with no live process behind it
/// fails here and is reclaimed by the next `start`).
pub fn live_session_at(socket: &Path) -> bool {
    if !socket.exists() {
        return false;
    }
    let Ok(mut stream) = UnixStream::connect(socket) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
    if write_msg(&mut stream, &Request::Ping).is_err() {
        return false;
    }
    matches!(read_msg::<Reply>(&stream), Ok(Reply::Pong))
}

/// True when a live session owns `apg_root`'s worktree DB.
pub fn live_session(apg_root: &Path) -> bool {
    live_session_at(&socket_path(apg_root))
}

/// Send one request and read its reply, connecting fresh each time.
fn send_request(socket: &Path, request: &Request) -> anyhow::Result<Reply> {
    let mut stream = UnixStream::connect(socket).map_err(|e| {
        anyhow::anyhow!(
            "could not connect to the session socket {}: {e}",
            socket.display()
        )
    })?;
    stream.set_read_timeout(Some(Duration::from_secs(60)))?;
    stream.set_write_timeout(Some(Duration::from_secs(60)))?;
    write_msg(&mut stream, request)?;
    read_msg::<Reply>(&stream).map_err(|e| anyhow::anyhow!("session read failed: {e}"))
}

/// Send with exactly one reconnect retry. The request carries a stable client
/// id, so a retry after a transport failure is at-most-once on the server
/// (its ledger returns the cached reply without re-applying). There is NO
/// direct-path fallback here — a forward that cannot reach a live coordinator is
/// an error the caller reports.
fn send_with_retry(socket: &Path, request: &Request) -> anyhow::Result<Reply> {
    match send_request(socket, request) {
        Ok(reply) => Ok(reply),
        Err(_) => {
            std::thread::sleep(Duration::from_millis(100));
            send_request(socket, request)
        }
    }
}

fn expect_ok(reply: Reply) -> anyhow::Result<String> {
    match reply {
        Reply::Ok { output } => Ok(output),
        Reply::Err { message } => anyhow::bail!("{message}"),
        other => anyhow::bail!("unexpected session reply: {other:?}"),
    }
}

/// The result of a routed read as a rendered string (CSV or JSON), ready to
/// print.
pub fn forward_query(apg_root: &Path, query: &str, json: bool) -> anyhow::Result<String> {
    let socket = socket_path(apg_root);
    let request = Request::Query {
        query: query.to_string(),
        json,
    };
    expect_ok(send_with_retry(&socket, &request)?)
}

// ---------------------------------------------------------------------------
// The write-back buffer (phase-01)
// ---------------------------------------------------------------------------

/// The in-memory write-back buffer entry for ONE buffered durable node-file
/// write or delete.
///
/// A live session admits each routed durable `apg node`/`apg edge` mutation
/// into a buffer of these records instead of rewriting the on-disk
/// `apg/layers/**` tree; the whole buffered set is flushed to disk (and
/// committed) at `apg session save` (`Coordinator::save`, phase-01 task-2, via
/// the buffered write surface in `layers::write`, task-9). Each record carries
/// the target node file's identity — layer dir, node type, name, and destination
/// path — so the buffer keys by identity and keeps only the last mutation for a
/// node (a later write/delete for an identity wins), and it carries the full
/// pending [`NodeFile`] content so a later mutation composes against the state
/// already buffered for that node. [`content`](Self::content) is `Some` for a
/// staged write and `None` for the delete marker (the node file is to be
/// removed). Plain data — no behavior; the buffer/save machinery that consumes
/// it is built by the later phase-01 tasks.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingChange {
    /// The node file's layer dir name (e.g. `requirements`) — the first
    /// identity segment and the first path segment under `apg/layers/`.
    pub layer: String,
    /// The node file's type (e.g. `requirement`) — Rust reserves `type`, so the
    /// field is `node_type`.
    pub node_type: String,
    /// The node file's name (the file-name stem) — its identity within
    /// `(layer, node_type)`.
    pub name: String,
    /// The destination node file under
    /// `apg/layers/<layer>/<node_type>/<name>.json`.
    pub path: PathBuf,
    /// The pending node-file content to stage (`Some`), or the delete marker
    /// (`None`) when this identity's file is to be removed.
    pub content: Option<NodeFile>,
}

// ---------------------------------------------------------------------------
// The coordinator (server side)
// ---------------------------------------------------------------------------

/// The session-scoped single writer: it owns `db.lbug` (opened once) and the
/// extended `specs.lock` flock (held for the session's life), binds the socket,
/// and serves routed mutations/reads in receive order. Its at-most-once ledger
/// maps a client id to the reply it already produced so a replay never
/// re-applies a mutation.
pub struct Coordinator {
    apg_root: PathBuf,
    socket_path: PathBuf,
    listener: Option<UnixListener>,
    /// The exclusively-owned DB handle — ONE open/parse amortized across N
    /// mutations. The projection apply runs through this handle.
    db: Option<ArtifactDb>,
    /// The extended whole-sequence flock held for the session's life, so a
    /// non-routing direct writer can never RMW the same node files.
    _lock: Option<SpecLockGuard>,
    /// At-most-once ledger: a forwarded mutation id → the reply already sent.
    ledger: HashMap<String, Reply>,
    /// The receive-order queue of accepted requests (FIFO — the single writer
    /// applies mutations in the order it receives them).
    queue: VecDeque<Request>,
    /// The in-memory write-back buffer of admitted-but-unsaved durable
    /// node-file changes (phase-01): each routed durable mutation stages its
    /// [`PendingChange`] here instead of touching `apg/layers/**`; the whole
    /// buffered set becomes durable — atomically, in one commit — at
    /// [`save`](Self::save). Populated by admission/routing (phase-01 task-4).
    buffer: Vec<PendingChange>,
}

impl Coordinator {
    /// `apg session start`: reclaim any stale socket, take the extended flock,
    /// open (and own) `db.lbug` once, bind the worktree socket, and serve until
    /// an `end` request arrives.
    pub fn start(apg_root: &Path) -> anyhow::Result<()> {
        let apg_root = apg_root.to_path_buf();
        // One session per worktree DB: a live session refuses a second start.
        Self::reclaim_stale_socket(&apg_root)?;

        // Hold the SAME extended flock the direct path uses for the session's
        // life. A non-routing `apg node`/`apg edge` therefore waits instead of
        // RMW-ing a node file the coordinator is rewriting.
        let lock = acquire_spec_lock(&apg_root)?;

        // Own the DB exclusively, opened ONCE (the amortized open). A missing
        // DB (no scan yet) is allowed — the durable node files are
        // authoritative and the projection is skipped until a scan creates it.
        let db = Self::open_owned_db(&apg_root)?;

        let socket = socket_path(&apg_root);
        if let Some(parent) = socket.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let listener = UnixListener::bind(&socket).map_err(|e| {
            anyhow::anyhow!("could not bind session socket {}: {e}", socket.display())
        })?;
        eprintln!("apg session: listening on {}", socket.display());

        let mut coordinator = Coordinator {
            apg_root,
            socket_path: socket,
            listener: Some(listener),
            db,
            _lock: Some(lock),
            ledger: HashMap::new(),
            queue: VecDeque::new(),
            buffer: Vec::new(),
        };
        coordinator.serve()
    }

    /// Open the owned DB handle once (the amortized open) and emit the
    /// observable open marker. `None` when there is no `db.lbug` yet.
    fn open_owned_db(apg_root: &Path) -> anyhow::Result<Option<ArtifactDb>> {
        let db_path = apg_root.join(specs::TRANS).join("db.lbug");
        if !db_path.exists() {
            return Ok(None);
        }
        let db = ArtifactDb::open(apg_root)?;
        eprintln!("{DB_OPEN_MARKER}");
        Ok(Some(db))
    }

    /// Reclaim a leftover socket with no live process behind it (a crash/SIGKILL
    /// leaves the file but no listener, so connect fails). A socket with a live
    /// session behind it is a hard refusal (one session per worktree DB).
    pub fn reclaim_stale_socket(apg_root: &Path) -> anyhow::Result<()> {
        let socket = socket_path(apg_root);
        if !socket.exists() {
            return Ok(());
        }
        if UnixStream::connect(&socket).is_ok() {
            anyhow::bail!(
                "a session is already live on {} — only one session may own the worktree DB; stop it with `apg session end`",
                socket.display()
            );
        }
        std::fs::remove_file(&socket).map_err(|e| {
            anyhow::anyhow!(
                "could not reclaim stale session socket {}: {e}",
                socket.display()
            )
        })?;
        eprintln!("apg session: reclaimed stale socket {}", socket.display());
        Ok(())
    }

    /// Server-side shutdown: release the DB handle and the extended flock and
    /// remove the socket, so a later `start` (or a direct writer) can proceed.
    /// Every mutation's projection delta was already applied at admission, so
    /// there is NO end-of-session flush.
    pub fn end(&mut self) -> anyhow::Result<()> {
        self.db = None;
        self._lock = None;
        let _ = std::fs::remove_file(&self.socket_path);
        eprintln!("apg session: ended");
        Ok(())
    }

    /// `apg session save`: make the whole buffered set durable. The buffered
    /// [`PendingChange`]s are collected into the node-file `writes` (those with
    /// `content: Some(node)`) and `deletes` (those with `content: None`, by
    /// their destination `path`), flushed ATOMICALLY to
    /// `apg/layers/<layer>/<type>/<name>.json` with exactly ONE git commit
    /// ([`layers::write::write_through_with_deletes`]), then the staleness
    /// gate's recorded `scan_meta` is re-anchored ([`git::reanchor_scan_meta`],
    /// mirroring the direct path's steps 4–5) and the buffer is cleared.
    ///
    /// A clean buffer writes nothing, makes no commit, and clears nothing — a
    /// pure no-op. The DB projection is NOT run here: each mutation is
    /// projected into the live `db.lbug` at admission (phase-01 task-4), so
    /// save only makes the node files durable and re-anchors.
    pub fn save(&mut self) -> anyhow::Result<()> {
        if self.buffer.is_empty() {
            println!("Session saved: no pending changes");
            return Ok(());
        }

        let mut writes: Vec<NodeFile> = Vec::new();
        let mut deletes: Vec<PathBuf> = Vec::new();
        for change in &self.buffer {
            match &change.content {
                Some(node) => writes.push(node.clone()),
                None => deletes.push(change.path.clone()),
            }
        }

        // Atomic multi-file write/delete + exactly ONE commit — the single
        // durability point for the whole buffered set.
        layers::write::write_through_with_deletes(&self.apg_root, &writes, &deletes)?;

        // Re-anchor the staleness gate AFTER the commit (mirrors the direct
        // path's write_project_with steps 4–5): graph.jsonl only, never opens
        // db.lbug. A re-anchor failure degrades to a warning — the durable
        // write already landed.
        if let Err(e) =
            crate::git::reanchor_scan_meta(&self.apg_root, &crate::git::git_state(&self.apg_root))
        {
            eprintln!("apg: warning: could not re-anchor scan_meta after session save: {e:#}");
        }

        let saved = self.buffer.len();
        self.buffer.clear();
        println!("Session saved: {saved} change(s) in one commit");
        Ok(())
    }

    /// Serve routed mutations AND routed reads in receive order until an `end`
    /// request arrives. Single-threaded: one request is fully applied before the
    /// next is read, so mutations are applied exactly in the order received with
    /// one writer and no lost update.
    pub fn serve(&mut self) -> anyhow::Result<()> {
        let listener = self
            .listener
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("session has no bound listener"))?
            .try_clone()?;
        loop {
            let (mut stream, _) = listener.accept()?;
            let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
            let request = match read_msg::<Request>(&stream) {
                Ok(request) => request,
                Err(_) => continue,
            };
            // Receive order: queue, then drain FIFO.
            self.queue.push_back(request);
            while let Some(request) = self.queue.pop_front() {
                match request {
                    Request::Ping => write_msg(&mut stream, &Reply::Pong)?,
                    Request::End => {
                        write_msg(
                            &mut stream,
                            &Reply::Ok {
                                output: String::new(),
                            },
                        )?;
                        self.end()?;
                        return Ok(());
                    }
                    Request::Query { query, json } => {
                        let reply = self.handle_query(&query, json);
                        write_msg(&mut stream, &reply)?;
                    }
                    Request::Save => {
                        let reply = match self.save() {
                            Ok(()) => Reply::Ok {
                                output: "Session saved".to_string(),
                            },
                            Err(e) => Reply::Err {
                                message: format!("{e:#}"),
                            },
                        };
                        write_msg(&mut stream, &reply)?;
                    }
                    Request::Mutate {
                        client_id,
                        kind,
                        args,
                    } => {
                        // At-most-once: a replayed id returns the cached reply
                        // and is never re-applied. Clone the cached reply out
                        // first so the ledger borrow ends before the (mutable)
                        // admission below.
                        let cached = self.ledger.get(&client_id).cloned();
                        let reply = match cached {
                            Some(cached) => cached,
                            None => {
                                let reply = self.handle_mutation(&kind, &args);
                                self.ledger.insert(client_id, reply.clone());
                                reply
                            }
                        };
                        write_msg(&mut stream, &reply)?;
                    }
                }
            }
        }
    }

    /// Admit one routed durable mutation into the buffer and project it into
    /// the live DB. Crate-private: it is driven only by [`serve`](Self::serve)
    /// over the wire, and its `Reply` wire type stays internal. Integration
    /// crates reach the session through the public `forward_*`/`signal_end`/
    /// `socket_path` surface.
    pub(crate) fn handle_mutation(&mut self, kind: &str, args: &[String]) -> Reply {
        match self.apply_mutation(kind, args) {
            Ok(output) => Reply::Ok { output },
            Err(e) => Reply::Err {
                message: format!("{e:#}"),
            },
        }
    }

    /// Admit one routed durable mutation as a write-back change (phase-01
    /// task-4, SPEC `cli-session-write-back-buffer`).
    ///
    /// The sequence, over the session's ONE owned DB handle:
    ///
    /// 1. Build a [`layers::LayersOverlay`] from the current buffer, then build
    ///    the change with [`node_cmd::build_change_over`](crate::node_cmd::build_change_over)
    ///    so its existence checks and read-modify-write resolve against the
    ///    CUMULATIVE buffered state (an update/rm of a node written earlier in
    ///    the same unsaved run applies over its buffered content).
    /// 2. Validate the change against that cumulative base via
    ///    [`layers::write::validate_change_over`] — NOT the disk-only
    ///    [`layers::write::validate_change`] — so an edge to a node added
    ///    earlier in the run is accepted though its file is not on disk yet.
    /// 3. Stage the change into a tentative overlay and, from the effective
    ///    buffered node set and the touched-FQN delete set, project it into the
    ///    live `db.lbug` AT ADMISSION through [`layers::write::project_only`] —
    ///    NO `apg/layers/**` write and NO commit.
    /// 4. Only after the projection SUCCEEDS, commit the change to
    ///    `self.buffer` (last write/delete for an identity wins). A failure at
    ///    any earlier step leaves `self.buffer` unchanged and the DB
    ///    un-projected.
    ///
    /// `self.db` may be `None` when no `db.lbug` exists yet: the change is
    /// still built, validated, and buffered, and the projection is skipped.
    fn apply_mutation(&mut self, kind: &str, args: &[String]) -> anyhow::Result<String> {
        // (1) The cumulative buffered state as an overlay.
        let overlay = self.overlay_from_buffer()?;

        // (2) Build the change against the cumulative buffered state.
        let change = crate::node_cmd::build_change_over(&self.apg_root, kind, args, &overlay)?;

        // The on-disk store and the cumulative base this change applies over.
        let disk = layers::read_existing_nodes(&self.apg_root)?;
        let base = overlay.apply_to_base(&disk);

        // (3) Validate against the cumulative base, NOT the disk-only store.
        layers::write::validate_change_over(
            &self.apg_root,
            &base,
            &change.writes,
            &change.deletes,
        )?;

        // Resolve every identity this change touches BEFORE projecting, so the
        // projection and the buffer commit cannot disagree on identity.
        let mut write_ids: Vec<(Layer, String, String)> = Vec::with_capacity(change.writes.len());
        for w in &change.writes {
            write_ids.push((
                crate::node_cmd::resolve_layer(&w.layer)?,
                w.node_type.clone(),
                w.name.clone(),
            ));
        }
        let mut delete_ids: Vec<(Layer, String, String, PathBuf)> =
            Vec::with_capacity(change.deletes.len());
        for path in &change.deletes {
            let (layer, node_type, name) = self.identity_of_node_path(path)?;
            delete_ids.push((layer, node_type, name, path.clone()));
        }

        // (4) Stage the change into a TENTATIVE overlay and compute the
        // effective buffered node set and the projection delete set (every
        // touched/changed identity FQN ∪ every deleted FQN).
        let mut tentative = overlay.clone();
        for w in &change.writes {
            tentative.stage_write(w.clone())?;
        }
        for (layer, node_type, name, _) in &delete_ids {
            tentative.stage_delete(*layer, node_type, name);
        }
        let effective_nodes = tentative.apply_to_base(&disk);
        let delete_fqns = tentative.touched_fqns();

        // Project the effective buffered state into the live db.lbug at
        // admission. Skipped when no query index exists yet (the durable node
        // files are the system of record); project_only also carries the
        // graph.jsonl-absent fallback and the transient re-merge.
        if let Some(db) = self.db.as_ref() {
            layers::write::project_only(
                &self.apg_root,
                &effective_nodes,
                &delete_fqns,
                &|deletes, records| db.reingest_layers_on(deletes, records),
            )?;
        }

        // (5) Only after the projection SUCCEEDS, commit the change to the
        // buffer — last write/delete for an identity wins.
        for (w, (layer, node_type, name)) in change.writes.iter().zip(&write_ids) {
            self.upsert_buffer(*layer, node_type, name, Some(w.clone()));
        }
        for (layer, node_type, name, path) in &delete_ids {
            if path.exists() {
                // A durable file backs this identity: keep a delete marker so
                // the next save removes it.
                self.upsert_buffer(*layer, node_type, name, None);
            } else {
                // The identity only ever existed in the buffer (added earlier
                // in this unsaved run): add-then-rm nets to nothing on disk, so
                // drop the pending write rather than leave a delete marker for
                // a file that was never written.
                self.remove_buffer_entry(*layer, node_type, name);
            }
        }

        Ok(change.message)
    }

    /// Build the cumulative buffered state as a [`layers::LayersOverlay`]: each
    /// buffered write is a staged write, each buffered delete marker a staged
    /// delete. Empty when nothing is buffered (every identity resolves to disk).
    fn overlay_from_buffer(&self) -> anyhow::Result<layers::LayersOverlay> {
        let mut overlay = layers::LayersOverlay::new();
        for change in &self.buffer {
            match &change.content {
                Some(node) => overlay.stage_write(node.clone())?,
                None => {
                    let layer = crate::node_cmd::resolve_layer(&change.layer)?;
                    overlay.stage_delete(layer, &change.node_type, &change.name);
                }
            }
        }
        Ok(overlay)
    }

    /// Replace the buffered state for one identity (last write wins), or append
    /// a new [`PendingChange`]. A replaced entry keeps its original position.
    fn upsert_buffer(
        &mut self,
        layer: Layer,
        node_type: &str,
        name: &str,
        content: Option<NodeFile>,
    ) {
        let layer_dir = layer.layer_dir();
        if let Some(entry) = self
            .buffer
            .iter_mut()
            .find(|c| c.layer == layer_dir && c.node_type == node_type && c.name == name)
        {
            entry.content = content;
        } else {
            let path = layers::node_file_path(&self.apg_root, layer, node_type, name);
            self.buffer.push(PendingChange {
                layer: layer_dir.to_string(),
                node_type: node_type.to_string(),
                name: name.to_string(),
                path,
                content,
            });
        }
    }

    /// Drop any buffered entry for one identity (an add-then-rm that nets to
    /// nothing on disk).
    fn remove_buffer_entry(&mut self, layer: Layer, node_type: &str, name: &str) {
        let layer_dir = layer.layer_dir();
        self.buffer
            .retain(|c| !(c.layer == layer_dir && c.node_type == node_type && c.name == name));
    }

    /// Derive the `(layer, type, name)` identity of a node-file path under
    /// `apg/layers/` — the inverse of [`layers::node_file_path`], keying a
    /// change's `deletes` back to their buffer identity.
    fn identity_of_node_path(&self, path: &Path) -> anyhow::Result<(Layer, String, String)> {
        let rel = path
            .strip_prefix(self.apg_root.join(layers::LAYERS_DIR))
            .map_err(|_| {
                anyhow::anyhow!("node-file path {} is not under apg/layers/", path.display())
            })?;
        let mut comps = rel.components();
        let layer_dir = comps
            .next()
            .and_then(|c| c.as_os_str().to_str())
            .ok_or_else(|| anyhow::anyhow!("malformed node-file path {}", path.display()))?;
        let node_type = comps
            .next()
            .and_then(|c| c.as_os_str().to_str())
            .ok_or_else(|| anyhow::anyhow!("malformed node-file path {}", path.display()))?;
        let file = comps
            .next()
            .and_then(|c| c.as_os_str().to_str())
            .ok_or_else(|| anyhow::anyhow!("malformed node-file path {}", path.display()))?;
        let name = file.strip_suffix(".json").ok_or_else(|| {
            anyhow::anyhow!("node-file path {} is not a .json file", path.display())
        })?;
        let layer = crate::node_cmd::resolve_layer(layer_dir)?;
        Ok((layer, node_type.to_string(), name.to_string()))
    }

    /// Serve a routed read against the session-held DB, rendered exactly like
    /// the direct `apg query` path.
    fn handle_query(&self, query: &str, json: bool) -> Reply {
        let Some(db) = self.db.as_ref() else {
            return Reply::Err {
                message: "the live session has no db.lbug (run `apg scan` first)".to_string(),
            };
        };
        match crate::render_query(&db.db, query, json) {
            Ok(output) => Reply::Ok { output },
            Err(e) => Reply::Err {
                message: format!("{e:#}"),
            },
        }
    }

    /// `apg session end` (client side): signal the live session to shut down.
    /// Idempotent — a missing/stale socket is reclaimed and reported as no live
    /// session. The client never releases a DB/socket it does not own.
    pub fn signal_end(apg_root: &Path) -> anyhow::Result<()> {
        let socket = socket_path(apg_root);
        if !socket.exists() {
            println!("no live session to end");
            return Ok(());
        }
        let result = UnixStream::connect(&socket).and_then(|mut stream| {
            stream.set_read_timeout(Some(Duration::from_secs(10)))?;
            write_msg(&mut stream, &Request::End)?;
            read_msg::<Reply>(&stream)
        });
        match result {
            Ok(_) => {
                println!("Session ended");
                Ok(())
            }
            Err(_) => {
                // Stale socket: reclaim so the next start can bind cleanly.
                let _ = std::fs::remove_file(&socket);
                println!("no live session to end (reclaimed stale socket)");
                Ok(())
            }
        }
    }

    /// The at-most-once client entry point: forward a node/edge mutation to the
    /// live session and return the coordinator's output message. A transport
    /// failure is reported (never a direct-path fallback).
    pub fn forward_mutation(
        apg_root: &Path,
        kind: &str,
        args: &[String],
    ) -> anyhow::Result<String> {
        let client_id = new_client_id();
        Self::forward_mutation_with_id(apg_root, &client_id, kind, args)
    }

    /// [`forward_mutation`](Self::forward_mutation) with an explicit client id —
    /// the primitive the at-most-once replay test drives directly (resend the
    /// same id and the coordinator returns the cached reply without
    /// re-applying).
    pub fn forward_mutation_with_id(
        apg_root: &Path,
        client_id: &str,
        kind: &str,
        args: &[String],
    ) -> anyhow::Result<String> {
        let socket = socket_path(apg_root);
        let request = Request::Mutate {
            client_id: client_id.to_string(),
            kind: kind.to_string(),
            args: args.to_vec(),
        };
        let reply = send_with_retry(&socket, &request).map_err(|e| {
            anyhow::anyhow!(
                "session forward failed for client id {client_id}: {e} — the coordinator is not reachable and the mutation was NOT applied locally (no silent fallback); retry once the session is reachable or run `apg session end`"
            )
        })?;
        expect_ok(reply)
    }
}
