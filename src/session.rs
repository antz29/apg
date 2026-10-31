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
//! durable mutation is applied to that database working state AT ADMISSION —
//! its row written, its edge halves projected — through the same handle, with
//! exactly the node files it changes marked dirty and a deleted node's row
//! detached as its identity enters the pending-removal ledger, while
//! `apg/layers/**` and git stay at the last saved state until
//! [`Coordinator::save`], so a routed read observes the admission while a direct
//! read observes the last saved state. Beside those marks the session keeps its
//! buffer, a pending-identity list of the identities the held database cannot
//! answer for alone — the node files this run created and the removals the
//! ledger holds — and never node-file content. Forwarded mutations carry a
//! client-generated id; the coordinator records applied ids so a replay is
//! at-most-once, and a failed forward is reported — the client never silently
//! falls back to the direct path mid-flight.

use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeSet, HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::artifacts::{ArtifactDb, SpecLockGuard, acquire_spec_lock};
use crate::layers::{self, Layer};
use crate::specs;

/// The session socket file name under `apg/.trans/`.
pub const SOCKET_NAME: &str = "session.sock";

/// The observable per-open marker the session prints to stderr; the
/// `live_session_amortizes_the_db_open_and_keeps_every_mutation_visible`
/// test counts it (an N-mutation session must open the DB materially fewer
/// than N times).
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
    /// The client asks the coordinator to make the held database's pending state
    /// durable: write its dirty rows, remove each pending-removal identity's
    /// node file, make exactly one commit, re-anchor `scan_meta`, then clear the
    /// marks — the protocol counterpart of [`Coordinator::save`].
    Save,
    /// Client-initiated abort: discard the database's pending state and abandon
    /// the session without making it durable — the protocol counterpart of the
    /// phase-02 abort lifecycle (discard the pending set, release the session,
    /// force a full scan when the run was dirty). Declared here so the wire
    /// protocol carries the request; the server arm is wired with that lifecycle.
    Abort,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) enum Reply {
    Pong,
    Ok {
        output: String,
        /// Write-time warnings produced by a routed durable mutation, carried
        /// from the coordinator to the client so the client can print them.
        /// `#[serde(default)]` keeps the wire compatible in both directions: a
        /// peer that predates this field deserializes it as an empty list, and
        /// a reply with no warnings serializes a harmless empty array.
        #[serde(default)]
        warnings: Vec<String>,
    },
    Err {
        message: String,
    },
}

/// The client-side result of a forwarded durable mutation: the coordinator's
/// human output message alongside the write-time warnings the mutation
/// produced. [`Coordinator::forward_mutation_with_id`] returns this instead of
/// a bare `String`, so the caller can print the warnings on its own stderr
/// while the write's result is left intact.
#[derive(Debug, Clone)]
pub struct ForwardedMutation {
    pub output: String,
    pub warnings: Vec<String>,
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

/// Discards a `.trans` directory's derived database: `db.lbug` **and its
/// `.wal`/`.shm` sidecars** (the suffixes [`crate::splice::publish`] asserts
/// on), and nothing else. Best-effort — an absent artifact is not an error.
///
/// Removing only `db.lbug` is not enough: a `db.lbug.wal` left behind by an
/// unclean exit makes the next `Database::new("db.lbug")` panic with a
/// database-id mismatch, so the sidecars must go too. This is the ONE discard
/// used by every forced-rebuild path: `apg scan --force`,
/// [`Coordinator::force_full_scan`], and `run_pipeline`'s stale-socket guard and
/// full-load branch. No caller keeps a second deletion list.
///
/// The `graph.jsonl` export is not part of this discard (its deletion is
/// phase 10's cut). The node files under `apg/layers/**` and every source file
/// are never touched here; the caller rebuilds the database from them.
pub(crate) fn discard_derived_index(trans_dir: &Path) {
    for name in ["db.lbug", "db.lbug.wal", "db.lbug.shm"] {
        let _ = std::fs::remove_file(trans_dir.join(name));
    }
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
        Reply::Ok { output, .. } => Ok(output),
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
// The pending-identity list
// ---------------------------------------------------------------------------

/// One pending node-file identity: the node file's layer dir, node type, name,
/// and the destination node file under `apg/layers/`.
///
/// A live session's list carries the identities the held database cannot decide
/// on its own: a node file this run created (no durable file backs its row) and a
/// pending removal the pending-removal ledger holds. Admission keys the list by
/// identity and keeps at most one entry per node, so a delete of a never-durable
/// identity drops its entry while a delete of a durable one records the pending
/// removal. The pending set itself is the held database's dirty rows plus its
/// pending-removal ledger, so this record carries no node content: the database
/// is the working state. Plain data — no behavior.
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
    /// The worktree's resolved project — the branch identity
    /// [`crate::git::repo_identity`] yields — populated once in
    /// [`Coordinator::start`] and used by admission to scope the held DB's
    /// transient-feedback read to the session's OWN project (`<project>/…`).
    /// `None` for a detached HEAD or a non-git checkout, mirroring the direct
    /// path's best-effort `repo_identity → branch` (which warns on nothing when
    /// there is no branch).
    project: Option<String>,
    socket_path: PathBuf,
    listener: Option<UnixListener>,
    /// The exclusively-owned DB handle, always present for the session's whole
    /// life — a session only starts against an existing `db.lbug` (`start`'s
    /// `open_owned_db` is fallible), and `end` merely drops this handle. ONE
    /// open/parse amortized across N mutations; the projection apply runs
    /// through it.
    db: ArtifactDb,
    /// The extended whole-sequence flock held for the session's life, so a
    /// non-routing direct writer can never RMW the same node files.
    _lock: Option<SpecLockGuard>,
    /// At-most-once ledger: a forwarded mutation id → the reply already sent.
    ledger: HashMap<String, Reply>,
    /// The receive-order queue of accepted requests (FIFO — the single writer
    /// applies mutations in the order it receives them).
    queue: VecDeque<Request>,
    /// The session's pending-identity list: the node files this run created and
    /// the pending removals the ledger holds. Admission keeps it current so a
    /// delete can tell a never-durable identity (drop) from a durable one
    /// (record the pending removal); it carries no node content, because the held
    /// database is the working state. [`save`](Self::save) and
    /// [`abort`](Self::abort) clear it at the durability boundary.
    buffer: Vec<PendingChange>,
}

/// What the accept loop must do after serving one connection.
enum ConnOutcome {
    /// The connection is done (or failed); keep serving.
    Continue,
    /// The peer asked for a clean `end`; the caller releases the session.
    End,
    /// The peer asked for `abort`; the caller discards the pending set and
    /// rebuilds.
    Abort,
}

impl Coordinator {
    /// `apg session start`: assert the project context, reclaim any stale
    /// socket, take the extended flock, open (and own) `db.lbug` once, seed its
    /// transient projection from `.trans`, resolve the worktree's project
    /// (best-effort), bind the worktree socket, and serve until an `end`
    /// request arrives.
    ///
    /// The project-context guard runs FIRST, before the socket reclaim, the
    /// flock, the DB open and the bind: a session is a durable mutation engine,
    /// so it is only legal inside a project worktree. A refused start therefore
    /// leaves any stale `session.sock` untouched and takes no DB.
    ///
    /// The guard admits a project worktree, so the absent-`db.lbug` refusal
    /// still follows it unchanged: a session always holds a database, and an
    /// absent `db.lbug` is a hard refusal naming `apg scan` (via
    /// [`open_owned_db`](Self::open_owned_db)), raised before the socket is
    /// bound, so a refused start leaves no socket bound and no flock held
    /// behind.
    ///
    /// The seed makes the held DB's transient rows (the plan store and the
    /// five tier mirrors — `Reviews`/`Gates`/`Satisfies` edges among them)
    /// current **once at start**, so DB-only admission can restore those
    /// edges without ever reading `.trans` afterwards. Like the DB open it is
    /// fail-closed and runs before the socket is bound: an absent `.trans`
    /// file is a no-op, but a present-but-malformed one refuses start before
    /// any socket is bound and leaves the held DB unchanged (the seed applies
    /// through one `BEGIN`/`COMMIT` that ROLLBACKs on error).
    pub fn start(apg_root: &Path) -> anyhow::Result<()> {
        // The project-context guard precedes every side effect: a refused start
        // must leave a stale socket in place and take no DB/flock.
        crate::git::require_project_context(apg_root)?;
        let apg_root = apg_root.to_path_buf();
        // One session per worktree DB: a live session refuses a second start.
        Self::reclaim_stale_socket(&apg_root)?;

        // Hold the SAME extended flock the direct path uses for the session's
        // life. A non-routing `apg node`/`apg edge` therefore waits instead of
        // RMW-ing a node file the coordinator is rewriting. The flock is taken
        // before the DB is opened because it is what serializes every
        // `db.lbug` access against a direct writer's reingest; on any `?`
        // refusal below the guard drops, releasing the flock.
        let lock = acquire_spec_lock(&apg_root)?;

        // Own the DB exclusively, opened ONCE (the amortized open). The session
        // always holds a database — there is no in-memory-only path: an absent
        // `db.lbug` refuses here, naming `apg scan`, before the socket is
        // bound, and `apg/layers/**` is never used as a DB-less fallback.
        let db = Self::open_owned_db(&apg_root)?;

        // Seed the held DB's transient projection ONCE, before the socket is
        // bound: merge the worktree's `.trans` plan-store + tier-mirror records
        // (the shared reader) with an EMPTY delete set, so the DB's transient
        // rows — `Reviews`/`Gates`/`Satisfies` edges among them — are current
        // for the session's whole life. DB-only admission can then restore
        // those edges without ever reading `.trans` again.
        //
        // Fail-closed: `append_transient_records` enumerates only EXISTING
        // files (an absent `.trans` is a benign no-op) and reads them ALL
        // before returning, erroring loudly on a present-but-malformed file
        // (its error already names the file + line). The `map_err` adds the
        // remedy without losing the file name. `reingest_layers_on` wraps the
        // apply in ONE BEGIN/COMMIT that ROLLBACKs on error, so a failed seed
        // leaves the held DB unchanged — and, because both run before
        // `UnixListener::bind`, a `?` abort leaves no socket bound and no flock
        // held (`lock`/`db` then drop).
        let mut transient: Vec<crate::schema::Record> = Vec::new();
        crate::layers::write::append_transient_records(&apg_root, &mut transient).map_err(|e| {
            anyhow::anyhow!(
                "{e}\nrepair or remove the offending .trans file, or run `apg scan` to re-project the transient records"
            )
        })?;
        db.reingest_layers_on(&std::collections::BTreeSet::new(), &transient)?;

        // The worktree's project (branch identity), resolved ONCE and stored so
        // admission can scope the held DB's transient-feedback read to this
        // session's own project. Best-effort exactly like the direct path:
        // a detached HEAD or a non-git checkout resolves to None (warns on
        // nothing), never a start refusal.
        let project = crate::git::repo_identity(&apg_root)
            .ok()
            .and_then(|identity| identity.branch);

        let socket = socket_path(&apg_root);
        if let Some(parent) = socket.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let listener = UnixListener::bind(&socket).map_err(|e| {
            anyhow::anyhow!("could not bind session socket {}: {e}", socket.display())
        })?;
        eprintln!("apg session: listening on {}", socket.display());

        let coordinator = Coordinator {
            apg_root,
            project,
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
    /// observable open marker. A session only starts against an existing
    /// `db.lbug`; an absent DB is a hard refusal naming `apg scan` as the
    /// remedy, and the returned handle is the non-optional owned DB.
    fn open_owned_db(apg_root: &Path) -> anyhow::Result<ArtifactDb> {
        let db_path = apg_root.join(specs::TRANS).join("db.lbug");
        if !db_path.exists() {
            anyhow::bail!(
                "{} does not exist — run `apg scan` first",
                db_path.display()
            );
        }
        let db = ArtifactDb::open(apg_root)?;
        eprintln!("{DB_OPEN_MARKER}");
        Ok(db)
    }

    /// Classify a leftover session socket and the database `start` is about to
    /// own, and recover from an unclean exit.
    ///
    /// * a socket is present and a live session answers a connect + `Ping` — a
    ///   hard refusal (one session per worktree DB);
    /// * a socket is present but fails connect + `Ping` — an UNCLEAN EXIT (a
    ///   crash/SIGKILL leaves the file but no listener): reclaim the stale
    ///   socket;
    /// * no socket, but the existing `db.lbug` carries **pending state** (a
    ///   dirty row or a pending-removal identity,
    ///   [`crate::artifacts::db::db_has_pending_state`]) or a **table shape
    ///   this binary does not recognise**
    ///   ([`crate::splice::seed::validate_compatible`], the same
    ///   `expected_schema`-versus-`extract_schema` comparison the splice seed
    ///   and the scan's freshness check ask) — an UNCLEAN EXIT too: the
    ///   database is ahead of the last saved state or belongs to another
    ///   binary, so it cannot be served.
    ///
    /// Every unclean verdict is this same classification the scan and query
    /// surfaces drive, and discards and rebuilds the derived `db.lbug` from the
    /// durable `apg/layers/**` node files via
    /// [`force_full_scan`](Self::force_full_scan) — the same recovery
    /// [`abort`](Self::abort) drives — so a killed run's phantom projection
    /// (admitted into the index but never made durable) is never served. A
    /// present, clean, shape-compatible database with no socket starts without
    /// a scan.
    ///
    /// An **absent** `db.lbug` is deliberately left to the caller's refusal:
    /// it is nothing to recover (and
    /// [`crate::splice::seed::validate_compatible`] would call it
    /// `Unreadable`), and `start` ([`open_owned_db`](Self::open_owned_db)) and
    /// `apg query` each raise their own hard refusal naming `apg scan`.
    pub fn reclaim_stale_socket(apg_root: &Path) -> anyhow::Result<()> {
        let socket = socket_path(apg_root);
        let socket_present = socket.exists();
        if socket_present && live_session_at(&socket) {
            anyhow::bail!(
                "a session is already live on {} — only one session may own the worktree DB; stop it with `apg session end`",
                socket.display()
            );
        }
        if socket_present {
            std::fs::remove_file(&socket).map_err(|e| {
                anyhow::anyhow!(
                    "could not reclaim stale session socket {}: {e}",
                    socket.display()
                )
            })?;
            eprintln!("apg session: reclaimed stale socket {}", socket.display());
        }

        let unclean = if socket_present {
            true
        } else {
            let db = crate::splice::seed::db_path(apg_root);
            if !db.exists() {
                // Not classified: an absent database is the caller's refusal, not
                // an unclean exit. Probing it would call it `Unreadable` and
                // rebuild one behind that refusal.
                false
            } else if crate::artifacts::db::db_has_pending_state(apg_root) {
                eprintln!(
                    "apg session: {} carries pending state — rebuilding it from apg/layers/**",
                    db.display()
                );
                true
            } else if crate::splice::seed::validate_compatible(&db).is_err() {
                eprintln!(
                    "apg session: {} has an unrecognised table shape — rebuilding it from apg/layers/**",
                    db.display()
                );
                true
            } else {
                false
            }
        };
        if unclean {
            Self::force_full_scan(apg_root)
        } else {
            Ok(())
        }
    }

    /// Server-side shutdown: consume the coordinator, dropping the
    /// non-optional held DB handle as it releases the extended flock and
    /// removes the socket, so a later `start` (or a direct writer) can proceed.
    /// Every mutation's projection delta was already applied at admission, so
    /// there is NO end-of-session flush.
    pub fn end(mut self) -> anyhow::Result<()> {
        self._lock = None;
        let _ = std::fs::remove_file(&self.socket_path);
        eprintln!("apg session: ended");
        Ok(())
    }

    /// Render the held database's pending state for a refused `end`: the count
    /// — its dirty rows
    /// ([`crate::artifacts::ArtifactDb::dirty_node_files_from_db`]) plus its
    /// pending-removal ledger identities
    /// ([`crate::artifacts::ArtifactDb::pending_removals`]) — then one
    /// `write <layer>.<type>.<name>` line per dirty row and one
    /// `delete <layer>.<type>.<name>` line per ledger identity, each block in
    /// the order its read yields (dirty rows by `(layer, node_type, name)`,
    /// ledger identities by `fqn`). That state is projected into the database
    /// while no `apg/layers/**` node file backs it, so [`end`](Self::end)
    /// refuses to release the session over it. A database whose pending state
    /// cannot be read is still a refusal — the message then carries the read
    /// error.
    fn pending_changes_message(&self) -> String {
        let (writes, deletes) = match (
            self.db.dirty_node_files_from_db(),
            self.db.pending_removals(),
        ) {
            (Ok(writes), Ok(deletes)) => (writes, deletes),
            (Err(e), _) | (_, Err(e)) => {
                return format!(
                    "session end refused: the database's pending state could not be read: {e:#}"
                );
            }
        };
        let mut msg = format!(
            "session end refused: {} pending change(s) are not durable — run `apg session save` to make them durable, or `apg session abort` to discard them",
            writes.len() + deletes.len()
        );
        for node in &writes {
            msg.push_str(&format!(
                "\n  write {}.{}.{}",
                node.layer, node.node_type, node.name
            ));
        }
        for removal in &deletes {
            msg.push_str(&format!("\n  delete {}", removal.fqn));
        }
        msg
    }

    /// `apg session save`: make the session's pending state durable. The exact
    /// write set is the held database's marked rows
    /// ([`crate::artifacts::ArtifactDb::dirty_node_files_from_db`]) and the
    /// exact delete set is its pending-removal ledger
    /// ([`crate::artifacts::ArtifactDb::pending_removals`]), each ledger
    /// identity (`<layer>.<type>.<name>`) mapped to the
    /// `apg/layers/<layer>/<type>/<name>.json` node file it names.
    ///
    /// Before anything is written, the two-input save boundary
    /// ([`layers::write::validate_save_model`], SPEC §8.1) judges the dirty rows
    /// as its structure scope and the held DB's full row set
    /// ([`crate::artifacts::ArtifactDb::node_files_from_db`] — the session's own
    /// projections, with every ledger-held identity already omitted) as its
    /// edge-pairing model: the effective post-save model itself, never an
    /// on-disk base with a staged set folded over it. An invalid set refuses the
    /// save BEFORE any write or commit, with the marks left pending, so the
    /// caller can complete the model or abort. Otherwise both sets go through
    /// [`layers::write::write_through_with_deletes`] in ONE atomic write of
    /// `apg/layers/**` plus exactly ONE git commit, the marks are then cleared
    /// ([`crate::artifacts::ArtifactDb::clear_pending_state`]) and the staleness
    /// gate's recorded `scan_meta` is re-anchored ([`git::reanchor_scan_meta`],
    /// mirroring the direct path's steps 4–5), so an immediately following save
    /// reads an empty pending set.
    ///
    /// An empty pending set — no marked row and no ledger identity — writes
    /// nothing, makes no commit, and reports no pending changes: a pure no-op.
    /// The DB projection is NOT run here: each admission already applied its
    /// projection to the live `db.lbug`, so save only makes the node files
    /// durable and re-anchors.
    pub fn save(&mut self) -> anyhow::Result<()> {
        let writes = self.db.dirty_node_files_from_db()?;
        let pending = self.db.pending_removals()?;
        if writes.is_empty() && pending.is_empty() {
            println!("Session saved: no pending changes");
            return Ok(());
        }

        // The held DB's full row set — this session's projections in, every
        // ledger-held identity out — is the edge-pairing model the dirty rows
        // are judged against.
        let held = self.db.node_files_from_db()?;
        layers::write::validate_save_model(&writes, &held)?;

        // Each pending identity names the node file this save removes.
        let deletes: Vec<PathBuf> = pending
            .iter()
            .map(|removal| {
                let (layer, node_type, name) = layers::parse_fqn(&removal.fqn)?;
                Ok(layers::node_file_path(
                    &self.apg_root,
                    layer,
                    &node_type,
                    &name,
                ))
            })
            .collect::<anyhow::Result<_>>()?;

        // Atomic multi-file write/delete + exactly ONE commit — the single
        // durability point for the pending set.
        layers::write::write_through_with_deletes(&self.apg_root, &writes, &deletes)?;

        // Drain the marks only once their node files are durable, so the next
        // save reads no pending state.
        self.db.clear_pending_state()?;

        // Re-anchor the staleness gate AFTER the commit (mirrors the direct
        // path's write_project_with steps 4–5): graph.jsonl only, never opens
        // db.lbug. A re-anchor failure degrades to a warning — the durable
        // write already landed.
        let state = crate::git::git_state(&self.apg_root);
        match crate::git::reanchor_scan_meta(&self.apg_root, &state) {
            Ok(()) => {
                // Reconcile the session's OWN DB `Scan` row to the SAME state,
                // through the handle it already holds — never a second `db.lbug`
                // open. The session always holds the database (a DB-less session
                // is refused at `start`, via `open_owned_db`), so the DB `Scan`
                // row is always present to refresh.
                if let Err(e) = self.db.refresh_scan_row(
                    state.sha.as_deref(),
                    state.sha.as_ref().map(|_| state.clean),
                    state.content_key.as_deref(),
                ) {
                    eprintln!(
                        "apg: warning: could not refresh the DB Scan row after session save: {e:#}"
                    );
                }
            }
            Err(e) => {
                eprintln!("apg: warning: could not re-anchor scan_meta after session save: {e:#}");
            }
        }

        // Clear the pending-identity list in step with the drained database: an
        // identity this run created leaves the list as its node file becomes
        // durable here, so a later `rm` of it reads the durable held DB row
        // (which now carries it) rather than the stale created-this-run entry.
        // Without the clear that later `rm` would drop its entry as never
        // durable and lose the deletion of a file this save just made durable.
        let saved = writes.len() + deletes.len();
        self.buffer.clear();
        println!("Session saved: {saved} change(s) in one commit");
        Ok(())
    }

    /// `apg session abort`: abandon the session without ever making its pending
    /// state durable. The discard decision is read from the **held database**
    /// BEFORE anything is released: a database carrying a dirty row
    /// ([`crate::artifacts::ArtifactDb::dirty_node_files_from_db`]) or a
    /// pending-removal ledger identity
    /// ([`crate::artifacts::ArtifactDb::pending_removals`]) holds an
    /// admission's projection that no `apg/layers/**` node file backs, so over
    /// that PENDING run it releases the session and then
    /// [`force_full_scan`](Self::force_full_scan) discards the derived index
    /// and rebuilds it from the durable node files — the unsaved projection is
    /// never served.
    ///
    /// A database carrying no pending state (no dirty row AND no ledger
    /// identity) has nothing unsaved projected into it, so the run simply
    /// releases the session: no discard and no rebuild.
    ///
    /// Consumes the coordinator, releasing it BEFORE the forced scan. Unlike
    /// [`end`](Self::end), the held DB handle is dropped first, so the stale
    /// mmap is off the index before the extended flock is released and the
    /// socket removed — the ordering the inline comment below restates. The
    /// scan opens (and locks) its own DB/socket, so holding either across it
    /// would self-deadlock — the scan's live-session guard would see this
    /// session's socket and refuse, and the stale handle would hold the index.
    ///
    /// The rebuild reuses the existing `apg scan` entry point
    /// ([`crate::cmd_scan`]), which drives the same
    /// [`crate::pipeline::run_pipeline`] a normal scan runs — ingestion is
    /// never reimplemented here. The derived index (`db.lbug` and its
    /// `graph.jsonl` export) is discarded FIRST so the scan cannot take its
    /// content-identity freshness fast path (or an incremental splice seeded
    /// from a database whose pending rows no node file backs) and reuse the
    /// unsaved state; a genuine full scan then rebuilds both from the durable
    /// node files and the scanned code.
    pub fn abort(mut self) -> anyhow::Result<()> {
        // Decide from the held DB BEFORE releasing it: a dirty row or a ledger
        // identity is an admission's projection that no node file backs, so an
        // aborted run carrying either owes a discard-and-rebuild. Nothing was
        // written to `apg/layers/**` or committed, so clearing the
        // pending-identity list is all abort discards itself.
        let pending = !self.db.dirty_node_files_from_db()?.is_empty()
            || !self.db.pending_removals()?.is_empty();
        self.buffer.clear();

        // Release exactly as `end` does, BEFORE the forced scan opens its own
        // DB handle / flock / socket: drop the owned DB handle first (no impl
        // `Drop`, so move the field out) so its stale mmap cannot hold the
        // index the scan is about to discard and rebuild, then release the
        // extended flock and remove the socket.
        drop(self.db);
        self._lock = None;
        let _ = std::fs::remove_file(&self.socket_path);

        if pending {
            Self::force_full_scan(&self.apg_root)?;
        }

        eprintln!("apg session: aborted");
        Ok(())
    }

    /// Force a full `apg scan` (the existing scan/pipeline entry point) that
    /// discards the derived index and rebuilds it from the durable node files.
    ///
    /// The caller has already released (or not yet acquired) the session's DB
    /// handle, flock and socket — [`abort`](Self::abort) releases them once the
    /// held database's own pending state (a dirty row or a pending-removal
    /// identity) decided the discard, and the start crash-check has not bound
    /// them yet. All this does is discard the derived `db.lbug` +
    /// `graph.jsonl`, so the scan cannot take a reuse fast path over the
    /// unsaved projection that database carried, then run the scan from the
    /// project root (the session owns the layout root, whose parent is the
    /// project dir). The scan's process-global `chdir` into `.trans` is
    /// restored so the serve loop's remaining lifetime is unaffected.
    ///
    /// The discard runs here, BEFORE [`crate::cmd_scan`] takes the spec lock —
    /// no live session exists at this point, so this path may delete without
    /// contending. It then calls `cmd_scan` WITHOUT `--force`: its scan still
    /// consults the shared fact store for the reuse decision (the warm probe is
    /// on) and may warm-reuse recorded facts, whereas `apg scan --force` never
    /// consults the store for that decision. Both paths leave the store itself
    /// in place and never touch `apg/layers/**`; only the derived `.trans`
    /// artifacts are discarded, so the rebuild sources the authored graph from
    /// the node files, never from the discarded database.
    fn force_full_scan(apg_root: &Path) -> anyhow::Result<()> {
        // Discard the derived index and its export: with no `db.lbug` the scan
        // freshness fast path cannot fire, and with no `graph.jsonl` there is
        // no previous export to splice from — the scan is forced to rebuild
        // from the durable node files (`apg/layers/**`) and freshly scanned
        // code. The node files themselves are the system of record and are
        // never touched.
        let trans = apg_root.join(specs::TRANS);
        discard_derived_index(&trans);

        // Reuse the full `crate::cmd_scan` entry point here rather than calling
        // `pipeline::run_pipeline` directly: `run_pipeline` consumes a prebuilt
        // record stream (its frontend/parquet load input) and cannot reconstruct
        // the index from the durable node files alone — the scan produces that
        // stream (spawning the frontends) itself. Deliberate reuse, not an
        // oversight.
        let project_dir = apg_root.parent().unwrap_or(apg_root).to_path_buf();
        let previous = std::env::current_dir()?;
        let result = crate::cmd_scan(&[project_dir.display().to_string()]);
        let _ = std::env::set_current_dir(previous);
        result
    }

    /// Serve routed mutations AND routed reads in receive order until an `end`
    /// request arrives. Single-threaded: one request is fully applied before the
    /// next is read, so mutations are applied exactly in the order received with
    /// one writer and no lost update.
    ///
    /// **A connection's I/O failure never ends the session.** A client that
    /// disconnects before its reply is delivered — a liveness probe that timed
    /// out, a killed process, an abandoned-then-retried request — yields a
    /// broken pipe on the reply write; that error is confined to that
    /// connection, and the accept loop keeps serving. A transient `accept`
    /// error is likewise retried or reported, never fatal. Only an explicit
    /// `end` (clean pending set) or `abort` request returns, so a routine probe
    /// can never orphan a healthy session.
    pub fn serve(mut self) -> anyhow::Result<()> {
        let listener = self
            .listener
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("session has no bound listener"))?
            .try_clone()?;
        loop {
            let (stream, _) = match listener.accept() {
                Ok(pair) => pair,
                // A signal-interrupted accept is retried; any other accept
                // failure is reported and the loop continues, so a transient
                // listener error cannot take down the coordinator.
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    eprintln!("apg session: accept failed: {e}");
                    continue;
                }
            };
            match self.handle_connection(stream) {
                ConnOutcome::Continue => {}
                ConnOutcome::End => {
                    if let Err(e) = self.end() {
                        eprintln!("apg session: end failed: {e:#}");
                    }
                    return Ok(());
                }
                ConnOutcome::Abort => {
                    if let Err(e) = self.abort() {
                        eprintln!("apg session: abort failed: {e:#}");
                    }
                    return Ok(());
                }
            }
        }
    }

    /// Serve exactly one accepted connection, returning what the accept loop
    /// must do next. No I/O error escapes: a peer that closes early, times out,
    /// or resets is confined to [`ConnOutcome::Continue`], so the coordinator's
    /// liveness never depends on a client reading its reply.
    fn handle_connection(&mut self, mut stream: UnixStream) -> ConnOutcome {
        // Bounded reads AND writes: a stalled peer can never block this
        // single-threaded loop indefinitely — the wait times out, the
        // connection is dropped, and the loop moves on.
        let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(30)));
        let request = match read_msg::<Request>(&stream) {
            Ok(request) => request,
            // The peer closed or timed out before sending: nothing to reply to.
            Err(_) => return ConnOutcome::Continue,
        };
        // Receive order: queue, then drain FIFO.
        self.queue.push_back(request);
        while let Some(request) = self.queue.pop_front() {
            match request {
                // A liveness probe whose peer has already gone away is not an
                // error: the write failure is discarded.
                Request::Ping => {
                    let _ = write_msg(&mut stream, &Reply::Pong);
                }
                Request::End => {
                    // The HELD DATABASE's own pending state decides — never the
                    // pending-identity list: a dirty row
                    // ([`crate::artifacts::ArtifactDb::dirty_node_files_from_db`])
                    // or a pending-removal ledger identity
                    // ([`crate::artifacts::ArtifactDb::pending_removals`]) is an
                    // admission's projection that no `apg/layers/**` node file
                    // backs, so releasing would serve, then lose, it. Read the
                    // decision ONCE per request and fail CLOSED: a state that
                    // cannot be read is a refusal too. Over an empty pending set
                    // nothing unsaved is projected, so the caller releases the DB
                    // handle, the extended flock and the socket.
                    let has_pending = match (
                        self.db.dirty_node_files_from_db(),
                        self.db.pending_removals(),
                    ) {
                        (Ok(writes), Ok(deletes)) => !writes.is_empty() || !deletes.is_empty(),
                        (Err(_), _) | (_, Err(_)) => true,
                    };
                    if !has_pending {
                        let _ = write_msg(
                            &mut stream,
                            &Reply::Ok {
                                output: "Session ended".to_string(),
                                warnings: Vec::new(),
                            },
                        );
                        return ConnOutcome::End;
                    }
                    let _ = write_msg(
                        &mut stream,
                        &Reply::Err {
                            message: self.pending_changes_message(),
                        },
                    );
                }
                Request::Query { query, json } => {
                    let reply = self.handle_query(&query, json);
                    let _ = write_msg(&mut stream, &reply);
                }
                Request::Save => {
                    let reply = match self.save() {
                        Ok(()) => Reply::Ok {
                            output: "Session saved".to_string(),
                            warnings: Vec::new(),
                        },
                        Err(e) => Reply::Err {
                            message: format!("{e:#}"),
                        },
                    };
                    let _ = write_msg(&mut stream, &reply);
                }
                Request::Abort => {
                    // Reply BEFORE the caller releases the session and runs the
                    // pending-state forced rebuild (which can take a full scan):
                    // the peer has already been answered, so the rebuild can
                    // never race a client read timeout into a broken pipe.
                    let _ = write_msg(
                        &mut stream,
                        &Reply::Ok {
                            output: "Session aborted".to_string(),
                            warnings: Vec::new(),
                        },
                    );
                    return ConnOutcome::Abort;
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
                    let _ = write_msg(&mut stream, &reply);
                }
            }
        }
        ConnOutcome::Continue
    }

    /// Admit one routed durable mutation into the database working state and
    /// project it into the live DB. Crate-private: it is driven only by
    /// [`serve`](Self::serve) over the wire, and its `Reply` wire type stays
    /// internal. Integration crates reach the session through the public
    /// `forward_*`/`signal_end`/`socket_path` surface.
    pub(crate) fn handle_mutation(&mut self, kind: &str, args: &[String]) -> Reply {
        match self.apply_mutation(kind, args) {
            // The change's write-time warnings ride alongside the output so the
            // client can print them; they never block the write (the change was
            // already admitted), and a replayed client id returns the cached
            // reply with the same warnings.
            Ok((output, warnings)) => Reply::Ok { output, warnings },
            Err(e) => Reply::Err {
                message: format!("{e:#}"),
            },
        }
    }

    /// Admit one routed durable mutation and project it into the live DB
    /// (phase-01 task-4, phase-02 task-1: DB-only admission).
    ///
    /// The sequence runs ENTIRELY over the session's ONE owned DB handle: it
    /// reads NO `apg/layers/**` node file and NO `apg/.trans/graph.jsonl`.
    ///
    /// 1. Reconstruct the database working state from the held DB
    ///    ([`crate::artifacts::ArtifactDb::node_files_from_db`], which already
    ///    carries this session's own projections and omits every ledger-held
    ///    identity). Build the change with
    ///    [`node_cmd::build_change_over`](crate::node_cmd::build_change_over)
    ///    over that base alone — no overlay folded in — so its existence checks
    ///    and read-modify-write resolve against the base directly. A routed
    ///    `node rm`'s outstanding-feedback warning is built from the held DB's
    ///    transient feedback set, scoped to the coordinator's resolved `project`
    ///    ([`crate::artifacts::ArtifactDb::feedback_records_from_db`]), never a
    ///    `.trans` read; a `None` project (detached HEAD / non-git) supplies an
    ///    empty set, so a routed rm warns on nothing — matching the direct path.
    /// 2. Validate the change against the same base via
    ///    [`layers::write::validate_change_over`] — NOT the disk-only
    ///    [`layers::write::validate_change`]. The two code-FQN universes come
    ///    from the same held DB
    ///    ([`crate::artifacts::ArtifactDb::code_universes_from_db`]), so an
    ///    `implemented-by` ref validates without opening a second handle or
    ///    reading the export.
    /// 3. Build ONLY this mutation's delta — the written node files' node +
    ///    out-edge records via [`layers::tree::records_for_nodes`], extended
    ///    with the incident in-edges read back from the held DB via
    ///    [`incident_edge_records_from_db`](crate::artifacts::ArtifactDb::incident_edge_records_from_db)
    ///    — and project exactly that delta, detaching exactly the touched
    ///    identity set, into the live `db.lbug` AT ADMISSION through
    ///    [`layers::write::project_only`] and the owned handle's
    ///    [`reingest_layers_on`](crate::artifacts::ArtifactDb::reingest_layers_on)
    ///    — NO `apg/layers/**` write, NO commit, and NO `.trans` re-read (the
    ///    start seed in [`Coordinator::start`](Self::start) established the
    ///    DB's transient rows).
    /// 4. Only after the projection SUCCEEDS, mark exactly the node files this
    ///    change rewrote dirty on the held DB
    ///    ([`crate::artifacts::ArtifactDb::mark_dirty`]) — `change.writes` is
    ///    already that exact set (an authored edge contributes both endpoints,
    ///    a code-target edge its source alone, and a delete its authored
    ///    neighbours through `node_rm_change`) — record each removed DURABLE
    ///    identity on the pending-removal ledger
    ///    ([`crate::artifacts::ArtifactDb::record_pending_removal`]), and clear
    ///    the pending-removal row of every identity that projection wrote back,
    ///    so the ledger agrees with the dirty mark this same admission set: the
    ///    next save writes the marked rows and deletes only the ledger's files.
    /// 5. Bring `self.buffer`, the pending-identity list, in step: an identity
    ///    this run created (no durable node file backs its row) enters the list,
    ///    while a durable one leaves it — its dirty mark carries the write — and
    ///    a durable delete adds its removal entry. A failure at any earlier step
    ///    leaves `self.buffer` unchanged and the DB un-projected.
    ///
    /// A delete of an identity that is durable — its row is the held DB's, and
    /// no created-this-run entry and no ledger row marks it otherwise — records
    /// the pending removal (and keeps the identity on the list), so the next save
    /// removes its node file even when the same unsaved run already updated it.
    /// A delete of an identity created earlier in this unsaved run drops its list
    /// entry and records nothing: no file was ever written, so it nets to nothing
    /// on disk. A re-add of an identity this run recorded as pending leaves the
    /// list and clears that ledger row at admission, so the next save writes the
    /// re-added file rather than deleting it.
    ///
    /// Returns the change's human message and its write-time warnings, so the
    /// caller can carry the warnings into the reply without them ever blocking
    /// the write.
    fn apply_mutation(
        &mut self,
        kind: &str,
        args: &[String],
    ) -> anyhow::Result<(String, Vec<String>)> {
        // (1) The database working state, reconstructed from the DB the session
        //     already holds: it already carries this session's own projections
        //     and omits every ledger-held identity, so no overlay is folded in.
        //     No `apg/layers/**` node file is read.
        let db_nodes = self.db.node_files_from_db()?;

        // The pending-removal ledger as this admission found it. An identity on
        // it is a durable node file a save still owes a delete, so it is never a
        // never-durable create — the distinction the pending-identity list and
        // the delete decision below turn on. Read once, before this admission
        // clears the row of every identity it writes back.
        let held: BTreeSet<String> = self
            .db
            .pending_removals()?
            .into_iter()
            .map(|removal| removal.fqn)
            .collect();

        // (2) Build the change over that base alone, so existence checks and
        //     read-modify-write resolve against the database working state
        //     directly.
        let base = &db_nodes;

        // The held DB's transient feedback set, scoped to this session's own
        // project, is the node-rm warning's source — admission reads no `.trans`.
        // A None project (detached HEAD / non-git) supplies an empty set, so a
        // routed rm warns on nothing, matching the direct path.
        let feedback: Vec<crate::schema::Record> =
            if kind == "node" && args.first().map(String::as_str) == Some("rm") {
                match self.project.as_deref() {
                    Some(project) => self.db.feedback_records_from_db(project)?,
                    None => Vec::new(),
                }
            } else {
                Vec::new()
            };

        let change =
            crate::node_cmd::build_change_over(base, &feedback, &self.apg_root, kind, args)?;

        // (3) The two code-reference universes from the same held DB handle —
        //     validate against them, NOT a `graph.jsonl` export.
        let (scanned, planned) = self.db.code_universes_from_db()?;
        layers::write::validate_change_over(
            &self.apg_root,
            base,
            &change.writes,
            &change.deletes,
            &scanned,
            &planned,
        )?;

        // Resolve every identity this change touches BEFORE projecting, so the
        // projection and the pending-identity list cannot disagree on identity.
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

        // (4) The exact identity FQN set this mutation touched: every written
        //     identity ∪ every deleted identity. Never the cumulative pending set.
        let mut touched: BTreeSet<String> = BTreeSet::new();
        for (layer, node_type, name) in &write_ids {
            touched.insert(layers::node_file::fqn(*layer, node_type, name));
        }
        for (layer, node_type, name, _) in &delete_ids {
            touched.insert(layers::node_file::fqn(*layer, node_type, name));
        }

        // (5) Build ONLY this mutation's delta: the written node files' node
        //     records + out-edge records (records_for_nodes), extended with the
        //     incident in-edges read back FROM THE HELD DB (a changed/removed
        //     identity's detach drops them; records_for_nodes only owns
        //     out-edges).
        let mut records = layers::tree::records_for_nodes(&change.writes)?;
        records.extend(self.db.incident_edge_records_from_db(&touched)?);

        // Project exactly that delta, detaching exactly the touched identities,
        // through the session's OWNED handle. No `.trans` re-read.
        layers::write::project_only(&touched, &records, &|deletes, records| {
            self.db.reingest_layers_on(deletes, records)
        })?;

        // (6) Only after the projection SUCCEEDS, mark dirty exactly the node
        //     files this change rewrote, so the next save serialises exactly
        //     this write set. `change.writes` IS that set — an authored edge
        //     dirties both endpoints, a code-target edge its source alone, and
        //     a delete the authored neighbours whose edge halves it rewrote.
        self.db.mark_dirty(&change.writes)?;

        // (7) The ledger agrees with the dirty marks: every identity this
        //     projection just wrote back is no longer pending, so a node removed
        //     and then re-added in one session is not left ledger-held and the
        //     next save writes its file instead of deleting it. A DELETE matches
        //     nothing for an identity that was never pending.
        {
            let conn = self.db.conn()?;
            for (layer, node_type, name) in &write_ids {
                conn.query(&format!(
                    "MATCH (n:PendingRemoval {{fqn: {}}}) DELETE n",
                    crate::artifacts::lit(&layers::node_file::fqn(*layer, node_type, name))
                ))?;
            }
        }

        // (8) Bring the pending-identity list in step with that admission.
        for (layer, node_type, name) in &write_ids {
            let fqn = layers::node_file::fqn(*layer, node_type, name);
            let listed = self.buffer.iter().any(|c| {
                c.layer == layer.layer_dir() && c.node_type == *node_type && c.name == *name
            });
            // Durability at FIRST admission: a listed identity is one this run
            // created unless the ledger held it (a removal a re-add wrote back),
            // and an unlisted one is durable exactly when the pre-projection DB
            // carried its row. A durable write leaves the list to its dirty mark.
            let durable = held.contains(&fqn)
                || (!listed
                    && db_nodes.iter().any(|n| {
                        n.layer == layer.layer_dir() && n.node_type == *node_type && n.name == *name
                    }));
            if durable {
                self.remove_buffer_entry(*layer, node_type, name);
            } else if !listed {
                self.upsert_buffer(*layer, node_type, name);
            }
        }
        for (layer, node_type, name, _path) in &delete_ids {
            let fqn = layers::node_file::fqn(*layer, node_type, name);
            // A listed identity the ledger does not hold was created in this
            // unsaved run: no node file was ever written for it, so its removal
            // nets to nothing on disk and its entry is dropped. Every other
            // identity is durable — the held DB carried its row, or the ledger
            // already held its removal — so the ledger records the removal the
            // next save owes and the identity stays listed.
            let created_this_run = !held.contains(&fqn)
                && self.buffer.iter().any(|c| {
                    c.layer == layer.layer_dir() && c.node_type == *node_type && c.name == *name
                });
            if created_this_run {
                self.remove_buffer_entry(*layer, node_type, name);
            } else {
                self.db.record_pending_removal(&fqn)?;
                self.upsert_buffer(*layer, node_type, name);
            }
        }

        Ok((change.message, change.warnings))
    }

    /// Record one identity on the pending-identity list, if it is not already
    /// there. It carries no node content — the held database is the working
    /// state — so its only job is the identity and destination path a later
    /// delete reads to tell a never-durable node from a durable one. A listed
    /// identity keeps its original position.
    fn upsert_buffer(&mut self, layer: Layer, node_type: &str, name: &str) {
        let layer_dir = layer.layer_dir();
        if self
            .buffer
            .iter()
            .any(|c| c.layer == layer_dir && c.node_type == node_type && c.name == name)
        {
            return;
        }
        let path = layers::node_file_path(&self.apg_root, layer, node_type, name);
        self.buffer.push(PendingChange {
            layer: layer_dir.to_string(),
            node_type: node_type.to_string(),
            name: name.to_string(),
            path,
        });
    }

    /// Drop the entry for one identity from the pending-identity list: an
    /// add-then-rm that nets to nothing on disk, or a re-added identity that is
    /// durable again.
    fn remove_buffer_entry(&mut self, layer: Layer, node_type: &str, name: &str) {
        let layer_dir = layer.layer_dir();
        self.buffer
            .retain(|c| !(c.layer == layer_dir && c.node_type == node_type && c.name == name));
    }

    /// Derive the `(layer, type, name)` identity of a node-file path under
    /// `apg/layers/` — the inverse of [`layers::node_file_path`], keying a
    /// change's `deletes` back to their pending identity.
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
        match crate::render_query(&self.db.db, query, json) {
            Ok(output) => Reply::Ok {
                output,
                warnings: Vec::new(),
            },
            Err(e) => Reply::Err {
                message: format!("{e:#}"),
            },
        }
    }

    /// `apg session end` (client side): signal the live session to shut down.
    /// Idempotent — a missing/stale socket is reclaimed and reported as no live
    /// session. The client never releases a DB/socket it does not own. Returns
    /// only once the server's socket is gone (bounded), so a caller never
    /// observes the reply-before-unlink window `is_fresh` reads as unfresh.
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
            Ok(reply) => {
                // Print the coordinator's reply: a clean `end` reports the
                // release, a database carrying pending state comes back as an Err
                // naming the pending changes (the session stays live).
                println!("{}", expect_ok(reply)?);
                // The server writes its reply BEFORE it releases the socket, so
                // wait (bounded) for the unlink: a present-but-dead socket is
                // what `is_fresh` reads as NOT fresh.
                let deadline = Instant::now() + Duration::from_secs(1);
                while socket.exists() {
                    if Instant::now() >= deadline {
                        anyhow::bail!(
                            "session stopped but its socket {} was not removed",
                            socket.display()
                        );
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Ok(())
            }
            Err(_) => {
                // A present-but-unreachable socket is an unclean exit. Reclaim
                // it and rebuild the derived index from the durable node files
                // exactly as a `start` crash-check does — the socket removal
                // alone would strand the killed run's phantom projection with
                // no later socket to detect it.
                Self::reclaim_stale_socket(apg_root)?;
                println!("no live session to end (reclaimed stale socket)");
                Ok(())
            }
        }
    }

    /// `apg session save` (client side): ask the live session to make the held
    /// database's pending state durable. Idempotent — a missing/stale socket is
    /// reclaimed and reported as no live session. A present-but-unreachable
    /// (stale) socket is an unclean exit: [`reclaim_stale_socket`] rebuilds the
    /// derived index from the durable node files exactly as a `start`
    /// crash-check does. The client never writes a DB or node file it does not
    /// own; it only carries the request and prints the coordinator's reply
    /// output.
    ///
    /// [`reclaim_stale_socket`]: Self::reclaim_stale_socket
    pub fn signal_save(apg_root: &Path) -> anyhow::Result<()> {
        let socket = socket_path(apg_root);
        if !socket.exists() {
            println!("no live session to save");
            return Ok(());
        }
        let result = UnixStream::connect(&socket).and_then(|mut stream| {
            stream.set_read_timeout(Some(Duration::from_secs(10)))?;
            write_msg(&mut stream, &Request::Save)?;
            read_msg::<Reply>(&stream)
        });
        match result {
            Ok(reply) => {
                println!("{}", expect_ok(reply)?);
                Ok(())
            }
            Err(_) => {
                // A present-but-unreachable socket is an unclean exit. Reclaim
                // it and rebuild the derived index from the durable node files
                // exactly as a `start` crash-check does — the socket removal
                // alone would strand the killed run's phantom projection with
                // no later socket to detect it.
                Self::reclaim_stale_socket(apg_root)?;
                println!("no live session to save (reclaimed stale socket)");
                Ok(())
            }
        }
    }

    /// `apg session abort` (client side): ask the live session to discard the
    /// held database's pending state and release the session. Idempotent — a
    /// missing/stale socket is reclaimed and reported as no live session. The
    /// client never releases a DB or node file it does not own; it only carries
    /// the request and prints the coordinator's reply output. Like
    /// [`signal_end`](Self::signal_end), it returns only once the server's
    /// socket is gone (bounded).
    pub fn signal_abort(apg_root: &Path) -> anyhow::Result<()> {
        let socket = socket_path(apg_root);
        if !socket.exists() {
            println!("no live session to abort");
            return Ok(());
        }
        let result = UnixStream::connect(&socket).and_then(|mut stream| {
            stream.set_read_timeout(Some(Duration::from_secs(10)))?;
            write_msg(&mut stream, &Request::Abort)?;
            read_msg::<Reply>(&stream)
        });
        match result {
            Ok(reply) => {
                println!("{}", expect_ok(reply)?);
                // The server writes its reply BEFORE it releases the socket, so
                // wait (bounded) for the unlink: a present-but-dead socket is
                // what `is_fresh` reads as NOT fresh.
                let deadline = Instant::now() + Duration::from_secs(1);
                while socket.exists() {
                    if Instant::now() >= deadline {
                        anyhow::bail!(
                            "session stopped but its socket {} was not removed",
                            socket.display()
                        );
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Ok(())
            }
            Err(_) => {
                // A present-but-unreachable socket is an unclean exit. Reclaim
                // it and rebuild the derived index from the durable node files
                // exactly as a `start` crash-check does — the socket removal
                // alone would strand the killed run's phantom projection with
                // no later socket to detect it.
                Self::reclaim_stale_socket(apg_root)?;
                println!("no live session to abort (reclaimed stale socket)");
                Ok(())
            }
        }
    }

    /// The at-most-once client entry point: forward a node/edge mutation to the
    /// live session and return the coordinator's [`ForwardedMutation`] (human
    /// output message plus the write-time warnings), so the caller can print
    /// the warnings on its own stderr while the write's result is left intact.
    /// A transport failure is reported (never a direct-path fallback).
    ///
    /// This is the warning-carrying shape produced by
    /// [`forward_mutation_with_id`](Self::forward_mutation_with_id), delegated
    /// with a freshly generated client id.
    pub fn forward_mutation(
        apg_root: &Path,
        kind: &str,
        args: &[String],
    ) -> anyhow::Result<ForwardedMutation> {
        let client_id = new_client_id();
        Self::forward_mutation_with_id(apg_root, &client_id, kind, args)
    }

    /// [`forward_mutation`](Self::forward_mutation) with an explicit client id —
    /// the primitive the at-most-once replay test drives directly (resend the
    /// same id and the coordinator returns the cached reply without
    /// re-applying). Carries the reply's write-time warnings alongside the
    /// output message.
    pub fn forward_mutation_with_id(
        apg_root: &Path,
        client_id: &str,
        kind: &str,
        args: &[String],
    ) -> anyhow::Result<ForwardedMutation> {
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
        match reply {
            Reply::Ok { output, warnings } => Ok(ForwardedMutation { output, warnings }),
            Reply::Err { message } => anyhow::bail!("{message}"),
            other => anyhow::bail!("unexpected session reply: {other:?}"),
        }
    }
}
