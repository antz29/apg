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
//! mutation's exact projection delta is applied synchronously as it completes
//! through that handle — there is no write-back buffer and no end-of-session
//! flush. Forwarded mutations carry a client-generated id; the coordinator
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
use crate::layers;
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
    /// Every mutation's projection delta was already applied write-through, so
    /// there is NO end-of-session flush.
    pub fn end(&mut self) -> anyhow::Result<()> {
        self.db = None;
        self._lock = None;
        let _ = std::fs::remove_file(&self.socket_path);
        eprintln!("apg session: ended");
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
                    Request::Mutate {
                        client_id,
                        kind,
                        args,
                    } => {
                        // At-most-once: a replayed id returns the cached reply
                        // and is never re-applied.
                        let reply = match self.ledger.get(&client_id) {
                            Some(cached) => cached.clone(),
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

    /// The whole durable write for one routed mutation, performed by the
    /// coordinator itself: build the node-file change (read-modify-write) and
    /// apply it — atomic write + exactly one commit — then apply the exact
    /// projection delta write-through through the session-held DB handle. The
    /// session already holds the extended flock, so nothing is re-acquired.
    pub fn handle_mutation(&self, kind: &str, args: &[String]) -> Reply {
        match self.apply_mutation(kind, args) {
            Ok(output) => Reply::Ok { output },
            Err(e) => Reply::Err {
                message: format!("{e:#}"),
            },
        }
    }

    fn apply_mutation(&self, kind: &str, args: &[String]) -> anyhow::Result<String> {
        let change = crate::node_cmd::build_change(&self.apg_root, kind, args)?;
        let db = self.db.as_ref();
        layers::write_project_with(
            &self.apg_root,
            &change.writes,
            &change.deletes,
            &|records| {
                match db {
                    // Write-through through the ONE session-held handle: the
                    // projection delta is applied as the mutation completes.
                    Some(db) => db.reingest_layers_on(records),
                    None => Ok(()),
                }
            },
        )?;
        Ok(change.message)
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
