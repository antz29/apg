//! Two-pass ingestion of unified-schema records into a [`Graph`], including the
//! canonical FQN renderer (SPEC §4).
//!
//! Pass 1 buffers node records and renders canonical FQNs (module verbatim,
//! struct `parent.name`, function `parent.name` / `parent.name(T1,T2)`, Go
//! `init` → `parent.init#<file-basename>`), building both `id → FQN` and
//! `FQN → Node` maps. Pass 2 resolves edge endpoints against those maps.
//!
//! The renderer fails loudly (panics) on any residual FQN collision between two
//! declarations of the same kind rather than silently overwriting. Cross-kind
//! collisions (a legal JVM package/type sharing a name, or a class in a
//! shadowed package colliding with a method of the shadowing class) resolve by
//! precedence: struct > module, struct > function.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::PathBuf;

use crate::classify::{ApgConfig, classify_code_type};
use crate::graph::{Graph, Location, Node, NodeKind};
use crate::schema::{Record, SCAN_HEAD};

pub struct IngestOptions<'a> {
    pub blacklist: &'a [String],
    pub language: &'a str,
    pub config: Option<&'a ApgConfig>,
}

/// Reuse/splice inputs for a win-B incremental assembly (phase-02 task-7): the
/// cached facts for files whose bytes AND resolution inputs are unchanged,
/// projected onto the current worktree. The target-only re-emitted spool is
/// ingested normally, then the cached units are spliced in.
pub struct Reuse<'a> {
    /// The store the cached units live in (already loaded).
    pub store: &'a crate::cache::FactStore,
    /// The cache key the units were stored under.
    pub cache_key: &'a crate::cache::CacheKey,
    /// Relative path (`/`-separated) -> `(lang, blob OID)` for units to reuse.
    pub files: Vec<(String, String, String)>,
    /// The scan root the cached units are re-based onto (absolute).
    pub reader_root: String,
    /// The languages whose frontend was **skipped entirely** this scan (an
    /// empty target set on a partial/incremental scan). Their global module
    /// scaffolding — the pure-intermediate modules and `Module -> Module`
    /// hierarchy that no per-file fact unit carries — is replayed from the store
    /// so the assembled graph (and the `graph.jsonl` rendered from it) equals a
    /// full rebuild (feedback-102).
    pub skipped_langs: BTreeSet<String>,
}

pub struct IngestReport {
    /// Number of records skipped due to blacklist filtering.
    pub skipped: u64,
    /// Number of module records dropped because a struct/function with the same
    /// FQN claimed that name (Java permits a package and a type to share a
    /// name; flat FQN space can't hold both, so the type wins).
    pub shadowed_modules: u64,
    /// Number of function records dropped because a struct with the same FQN
    /// claimed that name first (a class in a shadowed package can render the
    /// same FQN as a method of the class that shadowed it).
    pub shadowed_functions: u64,
}

#[derive(Debug, Clone)]
struct FuncDecl {
    id: String,
    parent: String,
    name: String,
    params: Vec<String>,
    file: String,
    path: String,
    start: u32,
    end: u32,
    start_line: u32,
    end_line: u32,
    /// Language this declaration was scanned under (a `lang_switch` record may
    /// set it mid-stream when a scan covers multiple languages).
    language: String,
}

fn is_blacklisted(fqn: &str, blacklist: &[String]) -> bool {
    blacklist.iter().any(|p| fqn.starts_with(p.as_str()))
}

fn file_basename(file: &str) -> String {
    std::path::Path::new(file)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| file.to_string())
}

/// `None` for empty strings, so spec/plan node fields that are absent stay
/// absent in the DB (queryable with `IS NULL`) instead of storing `""`.
fn opt(s: String) -> Option<String> {
    if s.is_empty() { None } else { Some(s) }
}

/// A spec/plan node carries no location, category, or code_type (SPEC R1).
fn spec_node(kind: NodeKind) -> Node {
    Node {
        kind,
        code_type: String::new(),
        ..Node::default()
    }
}

/// Claims `fqn` for declaration `id`, panicking if a different declaration
/// already rendered the same FQN. `seen` records the kind of the claimer so
/// the caller can resolve cross-kind collisions (type wins over module/function).
fn claim(seen: &mut HashMap<String, (String, NodeKind)>, id: &str, fqn: &str, kind: NodeKind) {
    if let Some((prev, _)) = seen.get(fqn) {
        if prev != id {
            panic!("FQN collision: `{fqn}` claimed by both `{prev}` and `{id}`");
        }
    } else {
        seen.insert(fqn.to_string(), (id.to_string(), kind));
    }
}

/// Renders the FQN of every function declaration (SPEC §4).
///
/// Declarations are grouped by `(parent, name)`: a singleton group renders
/// `parent.name`, an overloaded group renders `parent.name(T1,T2,...)` for every
/// member. Go `init` functions carry no signature, so each is rendered
/// `parent.init#<file-basename>` instead. The per-declaration language drives
/// the Go `init` special case (multi-language scans mix languages in one
/// buffer).
fn render_function_fqns(decls: &[FuncDecl]) -> Vec<(String, String)> {
    let mut groups: HashMap<(&str, &str), Vec<usize>> = HashMap::new();
    for (i, d) in decls.iter().enumerate() {
        groups
            .entry((d.parent.as_str(), d.name.as_str()))
            .or_default()
            .push(i);
    }

    let mut out = Vec::with_capacity(decls.len());
    for ((parent, name), idxs) in groups {
        if name == "init" && idxs.iter().any(|&i| decls[i].language == "go") {
            for i in idxs {
                out.push((
                    decls[i].id.clone(),
                    format!("{parent}.init#{}", file_basename(&decls[i].file)),
                ));
            }
        } else if idxs.len() == 1 {
            let d = &decls[idxs[0]];
            out.push((d.id.clone(), format!("{parent}.{name}")));
        } else {
            for i in idxs {
                let d = &decls[i];
                out.push((
                    d.id.clone(),
                    format!("{parent}.{name}({})", d.params.join(",")),
                ));
            }
        }
    }
    out
}

fn insert_node(graph: &mut Graph, fqn: String, node: Node) {
    // A real declaration replaces an unresolved-target placeholder (which lives
    // only in `graph.nodes`, not in `seen`), never the other way around.
    match graph.nodes.get(&fqn) {
        None => {}
        Some(existing) if existing.kind == NodeKind::UnresolvedTarget => {}
        // Scanner-replace (GraphModel-SPEC.md / PlanExecution-SPEC.md): a
        // present (scanned) node at an FQN a `planned` node holds supersedes
        // the planned placeholder. Edges are FQN-keyed, so incident edges
        // (`Anchors`, `ImplementedBy`, `Builds`, `Details`, `Contains`) re-point
        // to the real node automatically.
        Some(existing)
            if existing.status.as_deref() == Some("planned") && node.status.is_none() =>
        {
            graph.nodes.remove(&fqn);
        }
        // A planned node arriving at an FQN a present node already holds is
        // superseded — a plan never overwrites real code (the plan-writer is
        // rejected at authoring time; this is the ingest-side backstop).
        Some(_) if node.status.as_deref() == Some("planned") => return,
        Some(_) => panic!("duplicate project node FQN: `{fqn}`"),
    }
    graph.nodes.insert(fqn, node);
}

pub fn ingest(
    records: impl IntoIterator<Item = Record>,
    opts: &IngestOptions,
) -> (Graph, IngestReport) {
    ingest_records(records, opts, false)
}

/// [`ingest`] with win-B cached-fact reuse (phase-02 task-7).
///
/// On the incremental path the caller ingests the **target-only** re-emitted
/// spool here and passes the unaffected files' cached fact units via `reuse`.
/// The assembler splices those units into the SAME in-memory graph, so the
/// result is the full fact-spliced graph — exact node/edge/unresolved sets —
/// and is the single graph consumed downstream (win B and the phase-3 win-C
/// splice path both consume it).
///
/// Ordering: the spool (real, freshly resolved facts) is ingested first; cached
/// units are then merged, with a freshly-emitted node at an FQN always winning
/// over a cached one (the re-emitted target is authoritative). Cached units
/// provide the unaffected files' nodes and edges.
pub fn ingest_with_reuse(
    records: impl IntoIterator<Item = Record>,
    opts: &IngestOptions,
    reuse: Option<&Reuse>,
) -> (Graph, IngestReport) {
    let (mut graph, report) = ingest_records(records, opts, true);
    if let Some(reuse) = reuse {
        splice_cached(&mut graph, reuse);
        // One finalize after the cached facts land, so an edge to a cached
        // symbol is not pruned before its endpoint arrives.
        finalize_graph(&mut graph);
    }
    (graph, report)
}

/// Merges cached per-file fact units into an assembled graph. A fresh node at
/// an FQN wins; a cached node fills a gap. Cached edges are added only when
/// both endpoints exist after the merge (the same dangling-edge rule the
/// ingestor applies), and a cached edge already present is a no-op.
///
/// The merge is deliberately THREE passes — every unit's nodes first, then the
/// module records, then every unit's edges — so a cross-file edge whose
/// endpoints live in different units is never dropped by unit iteration order.
/// (A single nodes-then-edges pass per unit would lose a `calls`/`uses` edge
/// whose target unit had not been visited yet; the win-B fact splice must
/// preserve the exact full-scan edge set regardless of ordering.)
fn splice_cached(graph: &mut Graph, reuse: &Reuse) {
    let mut cached_modules: HashSet<String> = HashSet::new();
    let mut loaded: Vec<(crate::cache::FileFragment, String)> = Vec::new();
    for (rel, lang, oid) in &reuse.files {
        let Some((frag, stored_root)) = load_reusable(reuse.store, lang, rel, oid, reuse.cache_key)
        else {
            continue;
        };
        loaded.push((frag, stored_root));
    }

    // Pass 1: every unit's nodes (a fresh node at an FQN wins; a cached node
    // fills a gap; an UnresolvedTarget placeholder yields to a real node).
    for (frag, stored_root) in &loaded {
        let (modules, nodes, _) = frag.project(stored_root, &reuse.reader_root);
        for m in modules {
            cached_modules.insert(m);
        }
        for (fqn, node) in nodes {
            match graph.nodes.get(&fqn) {
                None => {
                    graph.nodes.insert(fqn, node);
                }
                Some(existing) if existing.kind == NodeKind::UnresolvedTarget => {
                    if node.kind != NodeKind::UnresolvedTarget {
                        graph.nodes.insert(fqn, node);
                    }
                }
                Some(_) => {
                    // A freshly re-emitted node (or another cached unit's node)
                    // already claims the FQN; keep it.
                }
            }
        }
    }

    // Pass 2: cached module records — a module declared by a reused unit must
    // exist for its Module→File `contains` edges to survive. Inserted when the
    // spool did not emit it.
    for m in cached_modules {
        if !graph.nodes.contains_key(&m) {
            insert_node(
                graph,
                m.clone(),
                Node {
                    kind: NodeKind::Module,
                    ..Node::default()
                },
            );
        }
    }

    // Pass 2b: the skipped languages' global module scaffolding (feedback-102).
    //
    // A language whose frontend was skipped re-emits nothing, so its
    // pure-intermediate modules and every `Module -> Module` hierarchy edge —
    // which no per-file fact unit can express — must be replayed from the store
    // or the assembled graph (the `graph.jsonl` export source) is structurally
    // incomplete even when the spliced DB keeps the seed's rows. Only a language
    // the orchestrator actually skipped is replayed, so a spawned language's
    // fresh scaffolding (emitted unconditionally, outside the emission filter)
    // is never shadowed by stale cache rows.
    for lang in &reuse.skipped_langs {
        let Some(scaffolding) = reuse.store.scaffolding(lang, reuse.cache_key) else {
            continue;
        };
        for m in &scaffolding.modules {
            graph.nodes.entry(m.clone()).or_insert_with(|| Node {
                kind: NodeKind::Module,
                ..Node::default()
            });
        }
        for (from, to) in &scaffolding.edges {
            graph.contains.insert((from.clone(), to.clone()));
        }
    }

    // Pass 3: every unit's edges, now that ALL endpoints exist.
    for (frag, stored_root) in &loaded {
        let (_, _, edges) = frag.project(stored_root, &reuse.reader_root);
        for e in edges {
            if !graph.nodes.contains_key(&e.from) || !graph.nodes.contains_key(&e.to) {
                continue;
            }
            match e.kind.as_str() {
                "contains" => {
                    graph.contains.insert((e.from, e.to));
                }
                "calls" => {
                    graph.calls.insert((e.from, e.to));
                }
                "uses" => {
                    graph.uses.insert((e.from, e.to));
                }
                "unresolved_call" => {
                    graph.unresolved_calls.insert((e.from, e.to, e.target_type));
                }
                "unresolved_use" => {
                    graph.unresolved_uses.insert((e.from, e.to));
                }
                _ => {}
            }
        }
    }
}

/// Loads a reusable cached unit for `(lang, rel, oid)` under `cache_key`. The
/// inputs digest is read from the file's current bytes' candidate unit when the
/// unit's own recorded inputs are needed; the caller keys reuse on the unit's
/// recorded inputs, so a unit is returned only when it exists for this exact
/// content+key. Input drift is checked by the store's index entry.
fn load_reusable(
    store: &crate::cache::FactStore,
    lang: &str,
    rel: &str,
    oid: &str,
    cache_key: &crate::cache::CacheKey,
) -> Option<(crate::cache::FileFragment, String)> {
    // `candidate` returns the unit regardless of input drift; the caller only
    // routes here for units it already accepted, so use it directly.
    store.candidate(lang, rel, oid, cache_key)
}

/// The core single-pass ingestor: renders canonical FQNs, inserts nodes, and
/// resolves edges. See [`ingest`] / [`ingest_with_reuse`].
///
/// `defer_finalize` suppresses the dangling-edge cleanup / edge validation
/// (the win-B path runs it once after the cached facts merge).
fn ingest_records(
    records: impl IntoIterator<Item = Record>,
    opts: &IngestOptions,
    defer_finalize: bool,
) -> (Graph, IngestReport) {
    let mut graph = Graph::default();
    let mut skipped = 0u64;
    let mut shadowed_modules = 0u64;
    let mut shadowed_functions = 0u64;
    let mut id_to_fqn: HashMap<String, String> = HashMap::new();
    let mut seen: HashMap<String, (String, NodeKind)> = HashMap::new();
    let mut funcs: Vec<FuncDecl> = Vec::new();
    // File nodes keyed by absolute path -> their parent module FQN.
    let mut files: HashMap<String, String> = HashMap::new();
    // Modules are buffered (not claimed/inserted immediately): a package and a
    // type may legally share a name in the JVM (`pkg.A` the package and `pkg.A`
    // the class, e.g. NetBeans' QA test-data project layout), and flat FQN space
    // can't represent both. Modules are inserted only after every struct and
    // function FQN is claimed, so a colliding module yields to the type.
    let mut modules: Vec<String> = Vec::new();
    // Current language: starts at the scan's language and switches when a
    // `lang_switch` record (injected by `apg scan` between frontend streams of
    // a multi-language scan) appears. Drives code_type classification and FQN
    // rendering per record.
    let mut lang: String = opts.language.to_string();

    // Stream the records in one pass: modules, structs, and unresolved targets
    // have deterministic FQNs and enter the graph immediately; functions are
    // buffered (overload grouping needs every declaration); edges are spooled
    // to a temp file and resolved in a second pass once ids are known. This
    // keeps memory bounded for large projects instead of buffering every
    // record (SPEC §6).
    static SPOOL_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let spool = std::env::temp_dir().join(format!(
        "apg-edge-spool-{}-{}-{}",
        std::process::id(),
        SPOOL_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    {
        let mut sw = BufWriter::new(std::fs::File::create(&spool).unwrap());
        for r in records {
            match r {
                Record::Module { fqn } => {
                    if is_blacklisted(&fqn, opts.blacklist) {
                        skipped += 1;
                        continue;
                    }
                    if !modules.contains(&fqn) {
                        modules.push(fqn);
                    }
                }
                Record::Struct {
                    id,
                    parent,
                    name,
                    path,
                    start,
                    end,
                    start_line,
                    end_line,
                } => {
                    let fqn = format!("{parent}.{name}");
                    claim(&mut seen, &id, &fqn, NodeKind::Struct);
                    id_to_fqn.insert(id.clone(), fqn.clone());
                    if is_blacklisted(&fqn, opts.blacklist) {
                        skipped += 1;
                        continue;
                    }
                    let code_type = classify_code_type(&path, &fqn, &lang, opts.config);
                    insert_node(
                        &mut graph,
                        fqn,
                        Node {
                            kind: NodeKind::Struct,
                            location: Some(Location {
                                path: PathBuf::from(&path),
                                start,
                                end,
                                start_line,
                                end_line,
                            }),
                            category: None,
                            code_type,
                            ..Node::default()
                        },
                    );
                }
                Record::Function {
                    id,
                    parent,
                    name,
                    params,
                    file,
                    path,
                    start,
                    end,
                    start_line,
                    end_line,
                } => funcs.push(FuncDecl {
                    id,
                    parent,
                    name,
                    params,
                    file,
                    path,
                    start,
                    end,
                    start_line,
                    end_line,
                    language: lang.clone(),
                }),
                Record::File {
                    path,
                    parent,
                    start_line,
                    end_line,
                } => {
                    // A file belongs to a module; if that module is blacklisted
                    // the file and everything in it is out of scope too.
                    if is_blacklisted(&parent, opts.blacklist) {
                        skipped += 1;
                        continue;
                    }
                    if !files.contains_key(&path) {
                        files.insert(path.clone(), parent.clone());
                        let code_type = classify_code_type(&path, &path, &lang, opts.config);
                        graph.nodes.insert(
                            path.clone(),
                            Node {
                                kind: NodeKind::File,
                                location: Some(Location {
                                    path: PathBuf::from(&path),
                                    start: 0,
                                    end: 0,
                                    start_line,
                                    end_line,
                                }),
                                category: None,
                                code_type,
                                ..Node::default()
                            },
                        );
                    }
                }
                Record::Unresolved { fqn, category } => {
                    graph.nodes.entry(fqn).or_insert_with(|| Node {
                        kind: NodeKind::UnresolvedTarget,
                        location: None,
                        category,
                        code_type: String::new(),
                        ..Node::default()
                    });
                }
                Record::LangSwitch { language } => {
                    lang = language;
                }
                // The scan_meta control record (emitted by `apg scan` ahead of
                // the whole stream) becomes the DB's single `Scan` node: the
                // git state the scan ran under. It leads the stream so a real
                // module can never claim the FQN first; fqn collisions still
                // panic loudly via `insert_node`.
                Record::ScanMeta {
                    git_sha,
                    git_clean,
                    content_key,
                    scanned_at,
                } => insert_node(
                    &mut graph,
                    SCAN_HEAD.to_string(),
                    Node {
                        git_sha,
                        git_clean,
                        content_key,
                        scanned_at: Some(scanned_at),
                        ..spec_node(NodeKind::Scan)
                    },
                ),
                // Spec/plan node records carry canonical FQNs (no opaque ids),
                // so they enter the graph immediately like modules and
                // unresolved targets. `insert_node` panics on a residual FQN
                // collision (SPEC R3), never silently overwrites.
                Record::Requirement {
                    fqn,
                    id,
                    title,
                    body,
                    feature,
                } => insert_node(
                    &mut graph,
                    fqn,
                    Node {
                        id: opt(id),
                        title: Some(title),
                        body: opt(body),
                        feature: opt(feature),
                        ..spec_node(NodeKind::Requirement)
                    },
                ),
                Record::PlannedNode {
                    fqn,
                    kind,
                    name,
                    parent,
                } => {
                    let kind = match kind.as_str() {
                        "module" => NodeKind::Module,
                        "file" => NodeKind::File,
                        "struct" => NodeKind::Struct,
                        "function" => NodeKind::Function,
                        other => panic!(
                            "planned_node kind must be module/file/struct/function, got `{other}`"
                        ),
                    };
                    insert_node(
                        &mut graph,
                        fqn.clone(),
                        Node {
                            kind,
                            name: opt(name),
                            status: Some("planned".to_string()),
                            ..spec_node(kind)
                        },
                    );
                    if !parent.is_empty() {
                        graph.contains.insert((parent, fqn));
                    }
                }
                // Tier-1/2/3 spec nodes (GraphModel-SPEC.md; PHASE_01): authored
                // via the spec tools, never scanned. All carry `name` (+ `body`);
                // Container carries a `kind`.
                Record::Stakeholder { fqn, name, body } => insert_node(
                    &mut graph,
                    fqn,
                    Node {
                        name: Some(name),
                        body: opt(body),
                        ..spec_node(NodeKind::Stakeholder)
                    },
                ),
                Record::Entity { fqn, name, body } => insert_node(
                    &mut graph,
                    fqn,
                    Node {
                        name: Some(name),
                        body: opt(body),
                        ..spec_node(NodeKind::Entity)
                    },
                ),
                Record::System { fqn, name, body } => insert_node(
                    &mut graph,
                    fqn,
                    Node {
                        name: Some(name),
                        body: opt(body),
                        ..spec_node(NodeKind::System)
                    },
                ),
                Record::Container {
                    fqn,
                    name,
                    kind,
                    body,
                } => insert_node(
                    &mut graph,
                    fqn,
                    Node {
                        name: Some(name),
                        sub_kind: opt(kind),
                        body: opt(body),
                        ..spec_node(NodeKind::Container)
                    },
                ),
                Record::Component { fqn, name, body } => insert_node(
                    &mut graph,
                    fqn,
                    Node {
                        name: Some(name),
                        body: opt(body),
                        ..spec_node(NodeKind::Component)
                    },
                ),
                // New-model tier catalog (apg-projects SPEC §3.1).
                Record::User { fqn, name, body } => insert_node(
                    &mut graph,
                    fqn,
                    Node {
                        name: Some(name),
                        body: opt(body),
                        ..spec_node(NodeKind::User)
                    },
                ),
                Record::Group {
                    fqn,
                    name,
                    attribute,
                    root,
                    body,
                } => insert_node(
                    &mut graph,
                    fqn,
                    Node {
                        name: Some(name),
                        attribute: opt(attribute),
                        root: opt(root),
                        body: opt(body),
                        ..spec_node(NodeKind::Group)
                    },
                ),
                Record::Value { fqn, name, body } => insert_node(
                    &mut graph,
                    fqn,
                    Node {
                        name: Some(name),
                        body: opt(body),
                        ..spec_node(NodeKind::Value)
                    },
                ),
                Record::Service { fqn, name, body } => insert_node(
                    &mut graph,
                    fqn,
                    Node {
                        name: Some(name),
                        body: opt(body),
                        ..spec_node(NodeKind::Service)
                    },
                ),
                Record::Person { fqn, name, body } => insert_node(
                    &mut graph,
                    fqn,
                    Node {
                        name: Some(name),
                        body: opt(body),
                        ..spec_node(NodeKind::Person)
                    },
                ),
                Record::Constraint {
                    fqn,
                    name,
                    body,
                    attaches_to,
                } => insert_node(
                    &mut graph,
                    fqn,
                    Node {
                        name: Some(name),
                        body: opt(body),
                        attaches_to: opt(attaches_to),
                        ..spec_node(NodeKind::Constraint)
                    },
                ),
                Record::Note { fqn, body, kind } => insert_node(
                    &mut graph,
                    fqn,
                    Node {
                        body: Some(body),
                        sub_kind: opt(kind),
                        ..spec_node(NodeKind::Note)
                    },
                ),
                Record::Feedback {
                    fqn,
                    body,
                    status,
                    disposition,
                } => insert_node(
                    &mut graph,
                    fqn,
                    Node {
                        body: Some(body),
                        status: opt(status),
                        disposition: opt(disposition),
                        ..spec_node(NodeKind::Feedback)
                    },
                ),
                Record::Plan {
                    fqn,
                    title,
                    strategy,
                } => insert_node(
                    &mut graph,
                    fqn,
                    Node {
                        title: Some(title),
                        strategy: opt(strategy),
                        ..spec_node(NodeKind::Plan)
                    },
                ),
                Record::PlanPhase {
                    fqn,
                    number,
                    title,
                    deliverable,
                    status,
                } => insert_node(
                    &mut graph,
                    fqn,
                    Node {
                        number: Some(number),
                        title: Some(title),
                        deliverable: opt(deliverable),
                        status: opt(status),
                        ..spec_node(NodeKind::PlanPhase)
                    },
                ),
                Record::Task {
                    fqn,
                    title,
                    kind,
                    tier,
                    status,
                    verb,
                    target,
                    new_fqn,
                } => insert_node(
                    &mut graph,
                    fqn,
                    Node {
                        title: Some(title),
                        sub_kind: opt(kind),
                        tier: opt(tier),
                        status: opt(status),
                        verb: opt(verb),
                        target: opt(target),
                        new_fqn: opt(new_fqn),
                        ..spec_node(NodeKind::Task)
                    },
                ),
                edge => write_edge(&mut sw, edge),
            }
        }
    }

    // Pass B: render function FQNs and insert function nodes. Structs claim
    // first (streaming pass), so a function whose FQN a struct already claimed
    // is shadowed: a class in a shadowed package can render the same FQN as a
    // method of the class that shadowed it (NetBeans QA test-data layout), and
    // flat FQN space can't hold both. The struct wins; the function is dropped
    // and its edges pruned as dangling. Function-vs-function still panics (a
    // genuine duplicate declaration is a scanner bug).
    for (id, fqn) in render_function_fqns(&funcs) {
        match seen.get(&fqn) {
            None => {
                seen.insert(fqn.clone(), (id.clone(), NodeKind::Function));
                id_to_fqn.insert(id, fqn);
            }
            Some((_, NodeKind::Struct)) => {
                shadowed_functions += 1;
            }
            Some((prev, _)) => {
                panic!("FQN collision: `{fqn}` claimed by both `{prev}` and `{id}`");
            }
        }
    }
    for f in &funcs {
        let Some(fqn) = id_to_fqn.get(&f.id).cloned() else {
            continue;
        };
        if is_blacklisted(&fqn, opts.blacklist) {
            skipped += 1;
            continue;
        }
        let code_type = classify_code_type(&f.path, &fqn, &f.language, opts.config);
        insert_node(
            &mut graph,
            fqn,
            Node {
                kind: NodeKind::Function,
                location: Some(Location {
                    path: PathBuf::from(&f.path),
                    start: f.start,
                    end: f.end,
                    start_line: f.start_line,
                    end_line: f.end_line,
                }),
                params: f.params.clone(),
                category: None,
                code_type,
                ..Node::default()
            },
        );
    }

    // Pass B2: insert module nodes. A module whose FQN was already claimed by a
    // struct or function is shadowed (legal Java package/type name sharing); it
    // is dropped and its contains edges are pruned by the dangling-edge cleanup
    // below rather than panicking.
    let mut shadowed: HashSet<String> = HashSet::new();
    for fqn in &modules {
        if seen.contains_key(fqn) {
            shadowed.insert(fqn.clone());
            shadowed_modules += 1;
            continue;
        }
        claim(&mut seen, fqn, fqn, NodeKind::Module);
        insert_node(
            &mut graph,
            fqn.clone(),
            Node {
                kind: NodeKind::Module,
                location: None,
                category: None,
                code_type: String::new(),
                ..Node::default()
            },
        );
    }

    // Pass B3: wire the File layer into containment. Neither endpoint needs the
    // edge spool: Module→File comes from each file record's parent module, and
    // File→unit is derived from every located node's path (a file contains all
    // structs and functions declared in it). A missing module parent (shadowed
    // or blacklisted) leaves the File node in place but prunes the Module→File
    // edge, like any other dangling-edge cleanup.
    for (file_path, parent) in &files {
        if parent.is_empty() {
            continue;
        }
        if graph
            .nodes
            .get(parent)
            .is_some_and(|n| n.kind == NodeKind::Module)
        {
            graph.contains.insert((parent.clone(), file_path.clone()));
        }
    }
    let located: Vec<(String, String)> = graph
        .nodes
        .iter()
        .filter_map(|(fqn, n)| {
            if matches!(n.kind, NodeKind::Struct | NodeKind::Function) {
                n.location
                    .as_ref()
                    .map(|l| (l.path.to_string_lossy().into_owned(), fqn.clone()))
            } else {
                None
            }
        })
        .collect();
    for (file_path, fqn) in located {
        if graph
            .nodes
            .get(&file_path)
            .is_some_and(|n| n.kind == NodeKind::File)
        {
            graph.contains.insert((file_path, fqn));
        }
    }

    // Pass C: resolve edge endpoints from the spool.
    let resolve =
        |s: &str| -> String { id_to_fqn.get(s).cloned().unwrap_or_else(|| s.to_string()) };
    {
        let mut er = EdgeReader {
            r: BufReader::new(std::fs::File::open(&spool).unwrap()),
        };
        while let Some(e) = er.next_edge() {
            match e {
                Record::Contains { from, to } => {
                    // A shadowed package cannot be a parent: dropping the module
                    // node re-roots its containment tree at the type of the same
                    // name, and a class does not contain a package beneath it
                    // (e.g. class `org.pkg.A` contains no such `org.pkg.A.deep`).
                    if shadowed.contains(&from) {
                        continue;
                    }
                    let a = resolve(&from);
                    let b = resolve(&to);
                    if is_blacklisted(&a, opts.blacklist) || is_blacklisted(&b, opts.blacklist) {
                        skipped += 1;
                        continue;
                    }
                    graph.contains.insert((a, b));
                }
                Record::Calls { from, to } => {
                    let a = resolve(&from);
                    let b = resolve(&to);
                    if is_blacklisted(&a, opts.blacklist) || is_blacklisted(&b, opts.blacklist) {
                        skipped += 1;
                        continue;
                    }
                    graph.calls.insert((a, b));
                }
                Record::Uses { from, to } => {
                    let a = resolve(&from);
                    let b = resolve(&to);
                    if is_blacklisted(&a, opts.blacklist) || is_blacklisted(&b, opts.blacklist) {
                        skipped += 1;
                        continue;
                    }
                    graph.uses.insert((a, b));
                }
                Record::UnresolvedCall {
                    from,
                    to,
                    target_type,
                } => {
                    let a = resolve(&from);
                    if is_blacklisted(&a, opts.blacklist) {
                        skipped += 1;
                        continue;
                    }
                    graph.unresolved_calls.insert((a, to, target_type));
                }
                Record::UnresolvedUse { from, to } => {
                    let a = resolve(&from);
                    if is_blacklisted(&a, opts.blacklist) {
                        skipped += 1;
                        continue;
                    }
                    graph.unresolved_uses.insert((a, to));
                }
                // Spec/plan edges reference canonical FQNs directly; `resolve`
                // is identity for them. Blacklisting prunes edges into excluded
                // code, like any other edge.
                Record::Details { from, to } => {
                    let (a, b) = (resolve(&from), resolve(&to));
                    if is_blacklisted(&a, opts.blacklist) || is_blacklisted(&b, opts.blacklist) {
                        skipped += 1;
                        continue;
                    }
                    graph.details.insert((a, b));
                }
                Record::Reviews { from, to } => {
                    let (a, b) = (resolve(&from), resolve(&to));
                    if is_blacklisted(&a, opts.blacklist) || is_blacklisted(&b, opts.blacklist) {
                        skipped += 1;
                        continue;
                    }
                    graph.reviews.insert((a, b));
                }
                Record::DependsOn { from, to } => {
                    let (a, b) = (resolve(&from), resolve(&to));
                    if is_blacklisted(&a, opts.blacklist) || is_blacklisted(&b, opts.blacklist) {
                        skipped += 1;
                        continue;
                    }
                    graph.depends_on.insert((a, b));
                }
                Record::Gates { from, to } => {
                    let (a, b) = (resolve(&from), resolve(&to));
                    if is_blacklisted(&a, opts.blacklist) || is_blacklisted(&b, opts.blacklist) {
                        skipped += 1;
                        continue;
                    }
                    graph.gates.insert((a, b));
                }
                Record::Satisfies { from, to } => {
                    let (a, b) = (resolve(&from), resolve(&to));
                    if is_blacklisted(&a, opts.blacklist) || is_blacklisted(&b, opts.blacklist) {
                        skipped += 1;
                        continue;
                    }
                    graph.satisfies.insert((a, b));
                }
                // Spine edges (GraphModel-SPEC.md; PHASE_01).
                Record::Drives { from, to } => {
                    let (a, b) = (resolve(&from), resolve(&to));
                    if is_blacklisted(&a, opts.blacklist) || is_blacklisted(&b, opts.blacklist) {
                        skipped += 1;
                        continue;
                    }
                    graph.drives.insert((a, b));
                }
                Record::Represents { from, to } => {
                    let (a, b) = (resolve(&from), resolve(&to));
                    if is_blacklisted(&a, opts.blacklist) || is_blacklisted(&b, opts.blacklist) {
                        skipped += 1;
                        continue;
                    }
                    graph.represents.insert((a, b));
                }
                // New-model §3.3 spec edges (apg-projects).
                Record::RealisedBy { from, to } => {
                    let (a, b) = (resolve(&from), resolve(&to));
                    if is_blacklisted(&a, opts.blacklist) || is_blacklisted(&b, opts.blacklist) {
                        skipped += 1;
                        continue;
                    }
                    graph.realised_by.insert((a, b));
                }
                Record::SpecImplementedBy { from, to } => {
                    let (a, b) = (resolve(&from), resolve(&to));
                    if is_blacklisted(&a, opts.blacklist) || is_blacklisted(&b, opts.blacklist) {
                        skipped += 1;
                        continue;
                    }
                    graph.spec_implemented_by.insert((a, b));
                }
                Record::Publishes { from, to } => {
                    let (a, b) = (resolve(&from), resolve(&to));
                    if is_blacklisted(&a, opts.blacklist) || is_blacklisted(&b, opts.blacklist) {
                        skipped += 1;
                        continue;
                    }
                    graph.publishes.insert((a, b));
                }
                Record::Subscribes { from, to } => {
                    let (a, b) = (resolve(&from), resolve(&to));
                    if is_blacklisted(&a, opts.blacklist) || is_blacklisted(&b, opts.blacklist) {
                        skipped += 1;
                        continue;
                    }
                    graph.subscribes.insert((a, b));
                }
                _ => unreachable!("non-edge record reached the edge pass"),
            }
        }
    }
    let _ = std::fs::remove_file(&spool);

    // The final prune/validate pass runs HERE for the full path; on the win-B
    // splice path the deferred cached facts are merged first, then the caller
    // re-runs `finalize_graph` (phase-02 task-7), so a freshly-emitted edge to a
    // cached symbol survives.
    if !defer_finalize {
        finalize_graph(&mut graph);
    }

    (
        graph,
        IngestReport {
            skipped,
            shadowed_modules,
            shadowed_functions,
        },
    )
}

/// The dangling-edge cleanup and spec/plan/spine edge validation (SPEC R2/R21,
/// §7). Split out so the win-B splice path can merge cached facts first and
/// then finalize once (phase-02 task-7) — an edge to a symbol the target-only
/// spool did not emit must not be pruned before the cached unit lands.
pub(crate) fn finalize_graph(graph: &mut Graph) {
    // Drop edges whose endpoints do not exist (dangling ids, blacklisted nodes).
    // Containment is a strict tree: the six code pairs (Module→Module,
    // Module→File, File→Struct, File→Function, Struct→Struct, Struct→Function),
    // the seven spec pairs (Spec→Requirement, Spec→Phase, Phase→Requirement,
    // Spec→Decision, Spec→NonGoal, Spec→AcceptanceCriterion,
    // Spec→VerificationItem), and the four plan pairs (Plan→PlanPhase,
    // PlanPhase→Task, PlanPhase→AcceptanceCriterion, PlanPhase→VerificationItem)
    // (SPEC §7, R2, R21).
    graph.contains.retain(|(a, b)| {
        graph.nodes.contains_key(a)
            && graph.nodes.contains_key(b)
            && valid_contains_pair(&graph.nodes[a].kind, &graph.nodes[b].kind)
    });
    graph.calls.retain(|(a, b)| {
        graph.nodes.contains_key(a)
            && graph.nodes.contains_key(b)
            && ((graph.nodes[a].kind == NodeKind::Function
                && graph.nodes[b].kind == NodeKind::Function)
                || (graph.nodes[a].kind == NodeKind::Service
                    && graph.nodes[b].kind == NodeKind::Service))
    });
    graph.uses.retain(|(a, b)| {
        graph.nodes.contains_key(a)
            && graph.nodes.contains_key(b)
            && ((graph.nodes[b].kind == NodeKind::Struct
                && matches!(graph.nodes[a].kind, NodeKind::Function | NodeKind::Struct))
                || (graph.nodes[a].kind == NodeKind::Person
                    && graph.nodes[b].kind == NodeKind::System))
    });
    graph.unresolved_calls.retain(|(a, b, _)| {
        graph.nodes.contains_key(a)
            && graph.nodes.contains_key(b)
            && graph.nodes[a].kind == NodeKind::Function
            && graph.nodes[b].kind == NodeKind::UnresolvedTarget
    });
    graph.unresolved_uses.retain(|(a, b)| {
        graph.nodes.contains_key(a)
            && graph.nodes.contains_key(b)
            && graph.nodes[b].kind == NodeKind::UnresolvedTarget
            && matches!(graph.nodes[a].kind, NodeKind::Function | NodeKind::Struct)
    });

    // Spec/plan edge validation (SPEC R2/R21). Spec records carry no ids, so
    // dangling here means a JSONL referenced a node that isn't in the graph.
    graph.details = filter_edges(graph, &graph.details, |g, a, b| {
        g.nodes.contains_key(a) && g.nodes.contains_key(b) && g.nodes[a].kind == NodeKind::Note
    });
    graph.reviews = filter_edges(graph, &graph.reviews, |g, a, b| {
        g.nodes.contains_key(a) && g.nodes.contains_key(b) && g.nodes[a].kind == NodeKind::Feedback
    });
    graph.depends_on = filter_edges(graph, &graph.depends_on, |g, a, b| {
        kind_is(g, a, NodeKind::Requirement) && kind_is(g, b, NodeKind::Requirement)
    });
    graph.gates = filter_edges(graph, &graph.gates, |g, a, b| {
        kind_is(g, a, NodeKind::PlanPhase) && kind_is(g, b, NodeKind::PlanPhase)
    });
    graph.satisfies = filter_edges(graph, &graph.satisfies, |g, a, b| {
        kind_is(g, a, NodeKind::PlanPhase) && kind_is(g, b, NodeKind::Requirement)
    });

    // Spine edges (GraphModel-SPEC.md; PHASE_01). `drives` runs Requirement →
    // Group/Entity/Value/Service; `represents` runs User → Entity and Entity →
    // Person (both endpoints must exist and carry the tier kinds).
    graph.drives = filter_edges(graph, &graph.drives, |g, a, b| {
        kind_is(g, a, NodeKind::Requirement)
            && matches!(
                g.nodes[b].kind,
                NodeKind::Group | NodeKind::Entity | NodeKind::Value | NodeKind::Service
            )
    });
    graph.represents = filter_edges(graph, &graph.represents, |g, a, b| {
        (kind_is(g, a, NodeKind::User) && kind_is(g, b, NodeKind::Entity))
            || (kind_is(g, a, NodeKind::Entity) && kind_is(g, b, NodeKind::Person))
    });

    // New-model §3.3 spec edges (apg-projects). RealisedBy runs Group/Entity/
    // Service → System/Container/Component; SpecImplementedBy runs System/
    // Container/Component → code; Publishes/Subscribes run Service → Entity.
    graph.realised_by = filter_edges(graph, &graph.realised_by, |g, a, b| {
        matches!(
            g.nodes[a].kind,
            NodeKind::Group | NodeKind::Entity | NodeKind::Service
        ) && is_solution_kind(g, b)
    });
    graph.spec_implemented_by = filter_edges(graph, &graph.spec_implemented_by, |g, a, b| {
        is_solution_kind(g, a)
            && g.nodes.contains_key(b)
            && matches!(
                g.nodes[b].kind,
                NodeKind::Module | NodeKind::File | NodeKind::Struct | NodeKind::Function
            )
    });
    graph.publishes = filter_edges(graph, &graph.publishes, |g, a, b| {
        kind_is(g, a, NodeKind::Service) && kind_is(g, b, NodeKind::Entity)
    });
    graph.subscribes = filter_edges(graph, &graph.subscribes, |g, a, b| {
        kind_is(g, a, NodeKind::Service) && kind_is(g, b, NodeKind::Entity)
    });
}

/// Whether a `(from, to)` kind pair is a valid `Contains` edge (SPEC §7, R2,
/// R21): the six code pairs, the two plan pairs, the §3.1 requirements tree
/// (Stakeholder/User/Requirement ⊃ Requirement), the plain-named domain
/// hierarchy (Group ⊃ Group/Entity/Value/Service), and the solution hierarchy
/// (System ⊃ Container ⊃ Component).
fn valid_contains_pair(a: &NodeKind, b: &NodeKind) -> bool {
    matches!(
        (a, b),
        (NodeKind::Module, NodeKind::Module)
            | (NodeKind::Module, NodeKind::File)
            | (NodeKind::File, NodeKind::Struct)
            | (NodeKind::File, NodeKind::Function)
            | (NodeKind::Struct, NodeKind::Struct)
            | (NodeKind::Struct, NodeKind::Function)
            | (NodeKind::Plan, NodeKind::PlanPhase)
            | (NodeKind::PlanPhase, NodeKind::Task)
            // New-model §3.3 `contains` rows (apg-projects): the requirements
            // tree and the plain-named domain hierarchy.
            | (NodeKind::Stakeholder, NodeKind::Requirement)
            | (NodeKind::User, NodeKind::Requirement)
            | (NodeKind::Requirement, NodeKind::Requirement)
            | (NodeKind::Group, NodeKind::Group)
            | (NodeKind::Group, NodeKind::Entity)
            | (NodeKind::Group, NodeKind::Value)
            | (NodeKind::Group, NodeKind::Service)
            | (NodeKind::System, NodeKind::Container)
            | (NodeKind::Container, NodeKind::Component)
    )
}

fn kind_is(graph: &Graph, fqn: &str, k: NodeKind) -> bool {
    graph.nodes.get(fqn).is_some_and(|n| n.kind == k)
}

/// The Solution-tier (C4) node kinds: System, Container, Component.
fn is_solution_kind(graph: &Graph, fqn: &str) -> bool {
    graph.nodes.get(fqn).is_some_and(|n| {
        matches!(
            n.kind,
            NodeKind::System | NodeKind::Container | NodeKind::Component
        )
    })
}

/// Returns the edges of `edges` that pass `keep`. A free function so each call
/// scopes its immutable borrow of `graph` (unlike a capturing closure, which
/// would block a later mutable borrow).
fn filter_edges(
    graph: &Graph,
    edges: &HashSet<(String, String)>,
    keep: impl Fn(&Graph, &str, &str) -> bool,
) -> HashSet<(String, String)> {
    edges
        .iter()
        .filter(|(a, b)| keep(graph, a, b))
        .cloned()
        .collect()
}

/// Binary spool format for edge records: one u8 tag (0 contains, 1 calls,
/// 2 uses, 3 unresolved_call, 4 unresolved_use, 5 details, 6 reviews,
/// 7 depends_on, 8 gates, 9 satisfies, 10 drives, 11 represents,
/// 12 realised_by, 13 spec_implemented_by, 14 publishes, 15 subscribes)
/// followed by three length-prefixed UTF-8 strings (from, to, target_type;
/// the last empty for most).
fn write_edge(w: &mut impl Write, r: Record) {
    match r {
        Record::Contains { from, to } => write_edge_fields(w, 0, &from, &to, ""),
        Record::Calls { from, to } => write_edge_fields(w, 1, &from, &to, ""),
        Record::Uses { from, to } => write_edge_fields(w, 2, &from, &to, ""),
        Record::UnresolvedCall {
            from,
            to,
            target_type,
        } => write_edge_fields(w, 3, &from, &to, &target_type),
        Record::UnresolvedUse { from, to } => write_edge_fields(w, 4, &from, &to, ""),
        Record::Details { from, to } => write_edge_fields(w, 5, &from, &to, ""),
        Record::Reviews { from, to } => write_edge_fields(w, 6, &from, &to, ""),
        Record::DependsOn { from, to } => write_edge_fields(w, 7, &from, &to, ""),
        Record::Gates { from, to } => write_edge_fields(w, 8, &from, &to, ""),
        Record::Satisfies { from, to } => write_edge_fields(w, 9, &from, &to, ""),
        Record::Drives { from, to } => write_edge_fields(w, 10, &from, &to, ""),
        Record::Represents { from, to } => write_edge_fields(w, 11, &from, &to, ""),
        Record::RealisedBy { from, to } => write_edge_fields(w, 12, &from, &to, ""),
        Record::SpecImplementedBy { from, to } => write_edge_fields(w, 13, &from, &to, ""),
        Record::Publishes { from, to } => write_edge_fields(w, 14, &from, &to, ""),
        Record::Subscribes { from, to } => write_edge_fields(w, 15, &from, &to, ""),
        other => unreachable!("non-edge record reached the edge spool: {other:?}"),
    }
}

fn write_edge_fields(w: &mut impl Write, tag: u8, a: &str, b: &str, c: &str) {
    w.write_all(&[tag]).unwrap();
    for s in [a, b, c] {
        w.write_all(&(s.len() as u32).to_le_bytes()).unwrap();
        w.write_all(s.as_bytes()).unwrap();
    }
}

struct EdgeReader<R: BufRead> {
    r: R,
}

impl<R: BufRead> EdgeReader<R> {
    fn next_edge(&mut self) -> Option<Record> {
        let mut tag = [0u8; 1];
        if self.r.read_exact(&mut tag).is_err() {
            return None;
        }
        let a = self.read_str();
        let b = self.read_str();
        let c = self.read_str();
        Some(match tag[0] {
            0 => Record::Contains { from: a, to: b },
            1 => Record::Calls { from: a, to: b },
            2 => Record::Uses { from: a, to: b },
            3 => Record::UnresolvedCall {
                from: a,
                to: b,
                target_type: c,
            },
            4 => Record::UnresolvedUse { from: a, to: b },
            5 => Record::Details { from: a, to: b },
            6 => Record::Reviews { from: a, to: b },
            7 => Record::DependsOn { from: a, to: b },
            8 => Record::Gates { from: a, to: b },
            9 => Record::Satisfies { from: a, to: b },
            10 => Record::Drives { from: a, to: b },
            11 => Record::Represents { from: a, to: b },
            12 => Record::RealisedBy { from: a, to: b },
            13 => Record::SpecImplementedBy { from: a, to: b },
            14 => Record::Publishes { from: a, to: b },
            15 => Record::Subscribes { from: a, to: b },
            t => panic!("bad edge spool tag: {t}"),
        })
    }

    fn read_str(&mut self) -> String {
        let mut len = [0u8; 4];
        self.r.read_exact(&mut len).unwrap();
        let n = u32::from_le_bytes(len) as usize;
        let mut buf = vec![0u8; n];
        self.r.read_exact(&mut buf).unwrap();
        String::from_utf8(buf).unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fd(id: &str, parent: &str, name: &str, params: &[&str], file: &str) -> FuncDecl {
        FuncDecl {
            id: id.to_string(),
            parent: parent.to_string(),
            name: name.to_string(),
            params: params.iter().map(|s| s.to_string()).collect(),
            file: file.to_string(),
            path: "/x/a.go".to_string(),
            start: 0,
            end: 1,
            start_line: 1,
            end_line: 1,
            language: "go".to_string(),
        }
    }

    fn srec(id: &str, parent: &str, name: &str, path: &str) -> Record {
        Record::Struct {
            id: id.to_string(),
            parent: parent.to_string(),
            name: name.to_string(),
            path: path.to_string(),
            start: 0,
            end: 1,
            start_line: 1,
            end_line: 1,
        }
    }

    fn frec(id: &str, parent: &str, name: &str, path: &str) -> Record {
        Record::Function {
            id: id.to_string(),
            parent: parent.to_string(),
            name: name.to_string(),
            params: vec![],
            file: path.to_string(),
            path: path.to_string(),
            start: 0,
            end: 1,
            start_line: 1,
            end_line: 1,
        }
    }

    fn file_rec(path: &str, parent: &str, end_line: u32) -> Record {
        Record::File {
            path: path.to_string(),
            parent: parent.to_string(),
            start_line: 1,
            end_line,
        }
    }

    fn fqns(decls: &[FuncDecl]) -> HashMap<String, String> {
        render_function_fqns(decls).into_iter().collect()
    }

    #[test]
    fn unique_function_keeps_simple_name() {
        let decls = [fd("n1", "pkg", "foo", &[], "/x/a.go")];
        let m = fqns(&decls);
        assert_eq!(m["n1"], "pkg.foo");
    }

    #[test]
    fn overloads_get_param_suffix() {
        let decls = [
            fd("n1", "pkg.C", "foo", &["int"], "/x/a.go"),
            fd("n2", "pkg.C", "foo", &["java.lang.String"], "/x/a.go"),
        ];
        let m = fqns(&decls);
        assert_eq!(m["n1"], "pkg.C.foo(int)");
        assert_eq!(m["n2"], "pkg.C.foo(java.lang.String)");
    }

    #[test]
    fn go_init_disambiguated_by_file() {
        let decls = [
            fd("n1", "pkg", "init", &[], "/x/a.go"),
            fd("n2", "pkg", "init", &[], "/x/b.go"),
        ];
        let m = fqns(&decls);
        assert_eq!(m["n1"], "pkg.init#a.go");
        assert_eq!(m["n2"], "pkg.init#b.go");
    }

    #[test]
    fn zero_param_overload_gets_empty_suffix() {
        let decls = [
            fd("n1", "pkg.C", "foo", &[], "/x/a.go"),
            fd("n2", "pkg.C", "foo", &["int"], "/x/a.go"),
        ];
        let m = fqns(&decls);
        assert_eq!(m["n1"], "pkg.C.foo()");
        assert_eq!(m["n2"], "pkg.C.foo(int)");
    }

    #[test]
    #[should_panic(expected = "FQN collision")]
    fn duplicate_fqn_panics() {
        let records = vec![
            Record::Module {
                fqn: "pkg".to_string(),
            },
            srec("n1", "pkg", "A", "/x/a.go"),
            srec("n2", "pkg", "A", "/x/b.go"),
        ];
        ingest(
            records,
            &IngestOptions {
                blacklist: &[],
                language: "go",
                config: None,
            },
        );
    }

    #[test]
    fn module_shadowed_by_type_does_not_panic() {
        // Java permits a package `org.pkg.A` and a class `org.pkg.A` to coexist.
        // The type wins; the shadowed module is dropped and its Module→File edge
        // pruned (the File node stays, containing the units declared in it),
        // while unrelated modules, files, and edges survive.
        let records = vec![
            Record::Module {
                fqn: "org.pkg".to_string(),
            },
            Record::Module {
                fqn: "org.pkg.A".to_string(),
            },
            Record::Module {
                fqn: "org.pkg.A.deep".to_string(),
            },
            srec("n1", "org.pkg", "A", "/x/A.java"),
            srec("n2", "org.pkg.A.deep", "B", "/y/B.java"),
            file_rec("/x/A.java", "org.pkg", 30),
            file_rec("/y/B.java", "org.pkg.A.deep", 40),
            Record::Contains {
                from: "org.pkg".to_string(),
                to: "org.pkg.A".to_string(),
            },
            Record::Contains {
                from: "org.pkg.A".to_string(),
                to: "org.pkg.A.deep".to_string(),
            },
        ];
        let (graph, report) = ingest(
            records,
            &IngestOptions {
                blacklist: &[],
                language: "java",
                config: None,
            },
        );
        assert_eq!(report.shadowed_modules, 1);
        // The class survives with its canonical FQN.
        assert!(graph.nodes.contains_key("org.pkg.A"));
        assert_eq!(graph.nodes["org.pkg.A"].kind, NodeKind::Struct);
        // The parent package and the package nested under the shadowed name
        // survive; the shadowed package itself is not present.
        assert!(graph.nodes.contains_key("org.pkg"));
        assert!(graph.nodes.contains_key("org.pkg.A.deep"));
        assert!(graph.nodes.contains_key("org.pkg.A.deep.B"));
        // Files survive with their own module·file·unit containment chains.
        assert!(graph.nodes.contains_key("/x/A.java"));
        assert!(graph.nodes.contains_key("/y/B.java"));
        assert_eq!(graph.nodes["/x/A.java"].kind, NodeKind::File);
        assert!(
            graph
                .contains
                .contains(&("org.pkg".to_string(), "/x/A.java".to_string()))
        );
        assert!(
            graph
                .contains
                .contains(&("/x/A.java".to_string(), "org.pkg.A".to_string()))
        );
        assert!(
            graph
                .contains
                .contains(&("org.pkg.A.deep".to_string(), "/y/B.java".to_string()))
        );
        assert!(
            graph
                .contains
                .contains(&("/y/B.java".to_string(), "org.pkg.A.deep.B".to_string()))
        );
        // But the shadowed package is not a parent: its Module→File edge and the
        // package chain through it are pruned.
        assert!(
            !graph
                .contains
                .contains(&("org.pkg.A".to_string(), "/x/A.java".to_string()))
        );
        assert!(
            !graph
                .contains
                .contains(&("org.pkg.A".to_string(), "org.pkg.A.deep".to_string()))
        );
    }

    #[test]
    fn function_shadowed_by_struct_does_not_panic() {
        // A class in a shadowed package (`p.A.test` in package `p.A`) renders
        // the same FQN as a method of the class `p.A`; the struct wins and the
        // function is dropped. Function-vs-function still panics.
        let records = vec![
            Record::Module {
                fqn: "p".to_string(),
            },
            Record::Module {
                fqn: "p.A".to_string(),
            },
            srec("n1", "p", "A", "/x/A.java"),
            srec("n2", "p.A", "test", "/y/test.java"),
            frec("n3", "p.A", "test", "/x/A.java"),
            frec("n5", "p.A", "other", "/x/A.java"),
            file_rec("/x/A.java", "p", 60),
            file_rec("/y/test.java", "p.A", 20),
            Record::Contains {
                from: "p".to_string(),
                to: "p.A".to_string(),
            },
            Record::Contains {
                from: "n1".to_string(),
                to: "n3".to_string(),
            },
            Record::Contains {
                from: "n1".to_string(),
                to: "n5".to_string(),
            },
        ];
        let (graph, report) = ingest(
            records,
            &IngestOptions {
                blacklist: &[],
                language: "java",
                config: None,
            },
        );
        // The struct `p.A.test` (from the shadowed package) wins over the
        // method `p.A.test`; the distinct method `p.A.other` survives.
        assert_eq!(report.shadowed_functions, 1);
        assert_eq!(report.shadowed_modules, 1);
        assert!(graph.nodes.contains_key("p.A.test"));
        assert_eq!(graph.nodes["p.A.test"].kind, NodeKind::Struct);
        assert!(graph.nodes.contains_key("p.A.other"));
        // The shadowed module is gone as a module — `p.A` exists only as the
        // winning struct — and the file in it survives but loses its module
        // parent chain (`p→p.A` module edge pruned).
        assert_eq!(graph.nodes["p.A"].kind, NodeKind::Struct);
        assert!(graph.nodes.contains_key("/x/A.java"));
        assert!(graph.nodes.contains_key("/y/test.java"));
        assert!(
            graph
                .contains
                .contains(&("p".to_string(), "/x/A.java".to_string()))
        );
        assert!(
            !graph
                .contains
                .contains(&("p".to_string(), "p.A".to_string()))
        );
        assert!(
            graph
                .contains
                .contains(&("/x/A.java".to_string(), "p.A".to_string()))
        );
        assert!(
            graph
                .contains
                .contains(&("/y/test.java".to_string(), "p.A.test".to_string()))
        );
        // The dropped function's containment (by struct and by file) is pruned;
        // the surviving function's edges stay.
        assert!(
            !graph
                .contains
                .contains(&("p.A".to_string(), "p.A.test".to_string()))
        );
        assert!(
            !graph
                .contains
                .contains(&("/x/A.java".to_string(), "p.A.test".to_string()))
        );
        assert!(
            graph
                .contains
                .contains(&("p.A".to_string(), "p.A.other".to_string()))
        );
        assert!(
            graph
                .contains
                .contains(&("/x/A.java".to_string(), "p.A.other".to_string()))
        );
    }

    #[test]
    fn module_replaces_unresolved_target() {
        // An unresolved placeholder node lives only in `graph.nodes`; a real
        // declaration (here: the `tests` package vs a bare type reference that
        // was emitted unresolved) replaces it instead of panicking.
        let records = vec![
            Record::Unresolved {
                fqn: "tests".to_string(),
                category: Some("unknown".to_string()),
            },
            Record::Module {
                fqn: "tests".to_string(),
            },
        ];
        let (graph, _) = ingest(
            records,
            &IngestOptions {
                blacklist: &[],
                language: "java",
                config: None,
            },
        );
        assert!(graph.nodes.contains_key("tests"));
        assert_eq!(graph.nodes["tests"].kind, NodeKind::Module);
    }

    #[test]
    fn edge_spool_roundtrip() {
        let edges = vec![
            Record::Contains {
                from: "n1".to_string(),
                to: "n2".to_string(),
            },
            Record::Calls {
                from: "n1".to_string(),
                to: "n2".to_string(),
            },
            Record::Uses {
                from: "n1".to_string(),
                to: "n2".to_string(),
            },
            Record::UnresolvedCall {
                from: "n1".to_string(),
                to: "java.lang.String.format".to_string(),
                target_type: String::new(),
            },
            Record::UnresolvedUse {
                from: "n1".to_string(),
                to: "java.util.List".to_string(),
            },
        ];
        let mut buf: Vec<u8> = Vec::new();
        for e in &edges {
            write_edge(&mut buf, e.clone());
        }
        let mut er = EdgeReader {
            r: std::io::Cursor::new(buf),
        };
        let mut out = Vec::new();
        while let Some(e) = er.next_edge() {
            out.push(e);
        }
        assert_eq!(out, edges);
    }

    #[test]
    fn lang_switch_classifies_and_renders_per_record() {
        // A multi-language scan merges several frontend streams, each preceded
        // by a `lang_switch` record. code_type classification uses each
        // record's language (ts test rules vs go test rules), and Go `init`
        // disambiguation applies only to Go declarations.
        let records = vec![
            Record::LangSwitch {
                language: "go".to_string(),
            },
            Record::Module {
                fqn: "github.com/x/y".to_string(),
            },
            srec("g1", "github.com/x/y", "Store", "/abs/store.go"),
            Record::Function {
                id: "g2".to_string(),
                parent: "github.com/x/y".to_string(),
                name: "init".to_string(),
                params: vec![],
                file: "/abs/store.go".to_string(),
                path: "/abs/store.go".to_string(),
                start: 1,
                end: 5,
                start_line: 1,
                end_line: 5,
            },
            file_rec("/abs/store.go", "github.com/x/y", 100),
            Record::LangSwitch {
                language: "ts".to_string(),
            },
            Record::Module {
                fqn: "@co/ui".to_string(),
            },
            srec("t1", "@co/ui.src.app", "App", "/proj/src/app.ts"),
            Record::Function {
                id: "t2".to_string(),
                parent: "@co/ui.src.app".to_string(),
                name: "init".to_string(),
                params: vec![],
                file: "/proj/src/app.ts".to_string(),
                path: "/proj/src/app.ts".to_string(),
                start: 1,
                end: 5,
                start_line: 1,
                end_line: 5,
            },
            file_rec("/proj/src/app.ts", "@co/ui", 30),
            file_rec("/proj/src/app.test.ts", "@co/ui", 20),
            srec("t3", "@co/ui.src.app", "Helper", "/proj/src/app.test.ts"),
        ];
        let (graph, report) = ingest(
            records,
            &IngestOptions {
                blacklist: &[],
                language: "go",
                config: None,
            },
        );
        assert_eq!(report.skipped, 0);
        // Go init is file-disambiguated; the TS function named `init` is not.
        assert!(graph.nodes.contains_key("github.com/x/y.init#store.go"));
        assert!(graph.nodes.contains_key("@co/ui.src.app.init"));
        // code_type is per-language: the Go store is src, the .test.ts file
        // (ts test rule) and its struct are test.
        assert_eq!(graph.nodes["/abs/store.go"].code_type, "src");
        assert_eq!(graph.nodes["/proj/src/app.ts"].code_type, "src");
        assert_eq!(graph.nodes["/proj/src/app.test.ts"].code_type, "test");
        assert_eq!(graph.nodes["@co/ui.src.app.Helper"].code_type, "test");
    }

    #[test]
    fn scan_meta_record_becomes_scan_node() {
        // `apg scan` leads the stream with a scan_meta record (git state at
        // scan time); the ingestor turns it into the `scan/HEAD` Scan node.
        let records = vec![
            Record::ScanMeta {
                git_sha: Some("abc123".to_string()),
                git_clean: Some(true),
                content_key: Some("deadbeef".to_string()),
                scanned_at: "2026-09-07T00:00:00Z".to_string(),
            },
            Record::Module {
                fqn: "github.com/x/y".to_string(),
            },
        ];
        let (graph, _) = ingest(
            records,
            &IngestOptions {
                blacklist: &[],
                language: "go",
                config: None,
            },
        );
        let n = &graph.nodes[SCAN_HEAD];
        assert_eq!(n.kind, NodeKind::Scan);
        assert_eq!(n.git_sha.as_deref(), Some("abc123"));
        assert_eq!(n.git_clean, Some(true));
        assert_eq!(
            n.content_key.as_deref(),
            Some("deadbeef"),
            "the stream's content-identity key must reach the DB Scan node"
        );
        assert_eq!(n.scanned_at.as_deref(), Some("2026-09-07T00:00:00Z"));

        // A non-git scan emits a scan_meta with no git fields; the node still
        // records the timestamp.
        let (graph, _) = ingest(
            vec![Record::ScanMeta {
                git_sha: None,
                git_clean: None,
                content_key: None,
                scanned_at: "2026-09-07T00:00:00Z".to_string(),
            }],
            &IngestOptions {
                blacklist: &[],
                language: "go",
                config: None,
            },
        );
        let n = &graph.nodes[SCAN_HEAD];
        assert_eq!(n.kind, NodeKind::Scan);
        assert_eq!(n.git_sha, None);
        assert_eq!(n.git_clean, None);
        assert_eq!(n.content_key, None);
    }

    #[test]
    fn end_to_end_ingest_resolves_edges() {
        let records = vec![
            Record::Module {
                fqn: "github.com/x/y".to_string(),
            },
            srec("n1", "github.com/x/y", "Store", "/abs/store.go"),
            frec("n2", "github.com/x/y", "Compute", "/abs/store.go"),
            Record::Function {
                id: "n2b".to_string(),
                parent: "github.com/x/y.Store".to_string(),
                name: "Get".to_string(),
                params: vec![],
                file: "/abs/store.go".to_string(),
                path: "/abs/store.go".to_string(),
                start: 1,
                end: 50,
                start_line: 1,
                end_line: 50,
            },
            file_rec("/abs/store.go", "github.com/x/y", 100),
            Record::Unresolved {
                fqn: "fmt.Errorf".to_string(),
                category: Some("stdlib".to_string()),
            },
            Record::Contains {
                from: "n1".to_string(),
                to: "n2b".to_string(),
            },
            Record::UnresolvedCall {
                from: "n2".to_string(),
                to: "fmt.Errorf".to_string(),
                target_type: String::new(),
            },
        ];
        let (graph, report) = ingest(
            records,
            &IngestOptions {
                blacklist: &[],
                language: "go",
                config: None,
            },
        );
        assert_eq!(report.skipped, 0);
        assert!(graph.nodes.contains_key("github.com/x/y.Store"));
        assert!(graph.nodes.contains_key("github.com/x/y.Compute"));
        assert!(graph.nodes.contains_key("github.com/x/y.Store.Get"));
        assert!(graph.nodes.contains_key("fmt.Errorf"));
        // File layer: module contains the file, the file contains its units,
        // and methods stay under their struct.
        assert!(
            graph
                .contains
                .contains(&("github.com/x/y".to_string(), "/abs/store.go".to_string()))
        );
        assert!(graph.contains.contains(&(
            "/abs/store.go".to_string(),
            "github.com/x/y.Store".to_string()
        )));
        assert!(graph.contains.contains(&(
            "/abs/store.go".to_string(),
            "github.com/x/y.Compute".to_string()
        )));
        assert!(!graph.contains.contains(&(
            "github.com/x/y".to_string(),
            "github.com/x/y.Store".to_string()
        )));
        assert!(graph.contains.contains(&(
            "github.com/x/y.Store".to_string(),
            "github.com/x/y.Store.Get".to_string()
        )));
        assert!(graph.unresolved_calls.contains(&(
            "github.com/x/y.Compute".to_string(),
            "fmt.Errorf".to_string(),
            String::new()
        )));
    }

    #[test]
    fn planned_node_lands_with_parent_contains() {
        // A plan-side planned_node record lands as an Implementation node with
        // `status: planned` (no location); a `parent` names the containing node
        // via a Contains edge (a valid File→Struct pair).
        let records = vec![
            Record::PlannedNode {
                fqn: "/abs/gateway.go".to_string(),
                kind: "file".to_string(),
                name: "gateway.go".to_string(),
                parent: String::new(),
            },
            Record::PlannedNode {
                fqn: "github.com/x/gateway".to_string(),
                kind: "struct".to_string(),
                name: "Gateway".to_string(),
                parent: "/abs/gateway.go".to_string(),
            },
        ];
        let (graph, _) = ingest(
            records,
            &IngestOptions {
                blacklist: &[],
                language: "go",
                config: None,
            },
        );
        // The planned node lands as a Struct with status=planned (no location).
        let fqn = "github.com/x/gateway".to_string();
        assert_eq!(graph.nodes[&fqn].kind, NodeKind::Struct);
        assert_eq!(graph.nodes[&fqn].status.as_deref(), Some("planned"));
        assert!(graph.nodes[&fqn].location.is_none());
        assert_eq!(
            graph.nodes["/abs/gateway.go"].status.as_deref(),
            Some("planned")
        );
        // The planned File→Struct containment lands (a valid Contains pair).
        assert!(
            graph
                .contains
                .contains(&("/abs/gateway.go".to_string(), fqn))
        );
    }

    #[test]
    fn scanner_replace_supersedes_planned_node_and_keeps_edges() {
        // The scanner-replace (PlanExecution-SPEC.md): a real declaration at a
        // planned FQN supersedes the planned node (status cleared, location
        // filled), and FQN-keyed incident edges (implemented-by) re-point to
        // the real node automatically — the why-to-code chain resolves to real
        // code.
        let records = vec![
            Record::Module {
                fqn: "github.com/x/y".to_string(),
            },
            // The scanner's real declaration of the planned FQN.
            srec("n1", "github.com/x/y", "Gateway", "/abs/gateway.go"),
            file_rec("/abs/gateway.go", "github.com/x/y", 50),
            // A solution component the planned node is implemented-by.
            Record::Component {
                fqn: "solution.component.checkout".to_string(),
                name: "checkout".to_string(),
                body: String::new(),
            },
            Record::SpecImplementedBy {
                from: "solution.component.checkout".to_string(),
                to: "github.com/x/y.Gateway".to_string(),
            },
            Record::PlannedNode {
                fqn: "github.com/x/y.Gateway".to_string(),
                kind: "struct".to_string(),
                name: "Gateway".to_string(),
                parent: String::new(),
            },
        ];
        let (graph, _) = ingest(
            records,
            &IngestOptions {
                blacklist: &[],
                language: "go",
                config: None,
            },
        );
        let fqn = "github.com/x/y.Gateway".to_string();
        let node = &graph.nodes[&fqn];
        // The real (scanned) node won: present, located, not planned.
        assert_eq!(node.kind, NodeKind::Struct);
        assert!(node.status.is_none(), "scanner-replace clears status");
        assert!(node.location.is_some(), "real node carries its location");
        // The implemented-by edge re-points to the realized node (FQN-keyed).
        assert!(
            graph
                .spec_implemented_by
                .contains(&("solution.component.checkout".to_string(), fqn.clone()))
        );
        // The real File→Struct containment landed from the scanner.
        assert!(
            graph
                .contains
                .contains(&("/abs/gateway.go".to_string(), fqn))
        );
    }

    #[test]
    fn blacklisted_nodes_and_edges_dropped() {
        let records = vec![
            Record::Module {
                fqn: "keep.mod".to_string(),
            },
            Record::Module {
                fqn: "drop.mod".to_string(),
            },
            srec("n1", "keep.mod", "A", "/x/a.go"),
            srec("n2", "drop.mod", "B", "/x/b.go"),
            file_rec("/x/a.go", "keep.mod", 10),
            file_rec("/x/b.go", "drop.mod", 10),
            Record::Contains {
                from: "drop.mod".to_string(),
                to: "/x/b.go".to_string(),
            },
        ];
        let (graph, report) = ingest(
            records,
            &IngestOptions {
                blacklist: &["drop.mod".to_string()],
                language: "go",
                config: None,
            },
        );
        assert!(report.skipped >= 3);
        assert!(graph.nodes.contains_key("keep.mod"));
        assert!(!graph.nodes.contains_key("drop.mod"));
        assert!(!graph.nodes.contains_key("drop.mod.B"));
        // A file whose parent module is blacklisted is dropped along with its
        // units; the surviving file keeps its module and unit edges.
        assert!(!graph.nodes.contains_key("/x/b.go"));
        assert!(graph.nodes.contains_key("/x/a.go"));
        assert!(
            graph
                .contains
                .contains(&("keep.mod".to_string(), "/x/a.go".to_string()))
        );
        assert!(
            graph
                .contains
                .contains(&("/x/a.go".to_string(), "keep.mod.A".to_string()))
        );
        assert!(!graph.contains.is_empty());
    }

    #[test]
    fn cached_cross_file_edges_survive_unit_order() {
        // Regression: the win-B fact splice merges ALL cached nodes before ANY
        // cached edges, so a cross-file `calls`/`uses` edge whose target unit is
        // visited later is not dropped. The reuse list is deliberately ordered
        // so the depending file comes FIRST (its callee lands in a later unit).
        use crate::cache::{CacheKey, FactStore, FileFragment, ScanConfigKey};
        use crate::graph::{Graph, Location, Node, NodeKind};
        use std::path::PathBuf;

        let dir = std::env::temp_dir().join(format!("apg-splice-order-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let cache_key = CacheKey::compute(&ScanConfigKey::default());
        let mut store = FactStore::at(dir.join("facts"));

        // Build a two-file graph: b/b.go.Later calls a/a.go.Leaf.
        let mut g = Graph::default();
        g.nodes.insert(
            "scratch".to_string(),
            Node {
                kind: NodeKind::Module,
                ..Node::default()
            },
        );
        for (fqn, path, kind) in [
            ("scratch/a.Leaf", "/w/a/a.go", NodeKind::Function),
            ("scratch/b.Later", "/w/b/b.go", NodeKind::Function),
        ] {
            g.nodes.insert(
                fqn.to_string(),
                Node {
                    kind,
                    location: Some(Location {
                        path: PathBuf::from(path),
                        start: 0,
                        end: 1,
                        start_line: 1,
                        end_line: 1,
                    }),
                    code_type: "src".into(),
                    ..Node::default()
                },
            );
        }
        for path in ["/w/a/a.go", "/w/b/b.go"] {
            g.nodes.insert(
                path.to_string(),
                Node {
                    kind: NodeKind::File,
                    location: Some(Location {
                        path: PathBuf::from(path),
                        start: 0,
                        end: 0,
                        start_line: 1,
                        end_line: 1,
                    }),
                    ..Node::default()
                },
            );
            g.contains.insert(("scratch".to_string(), path.to_string()));
        }
        g.contains
            .insert(("/w/a/a.go".to_string(), "scratch/a.Leaf".to_string()));
        g.contains
            .insert(("/w/b/b.go".to_string(), "scratch/b.Later".to_string()));
        g.calls
            .insert(("scratch/b.Later".to_string(), "scratch/a.Leaf".to_string()));

        for (abs, rel) in [("/w/a/a.go", "a/a.go"), ("/w/b/b.go", "b/b.go")] {
            let frag = FileFragment::from_graph(&g, abs, rel, &format!("oid-{rel}"), "go");
            store.put(&frag, "/w", &cache_key).unwrap();
        }

        // The reuse list puts b/b.go (the caller) BEFORE a/a.go (the callee).
        let reuse = Reuse {
            store: &store,
            cache_key: &cache_key,
            files: vec![
                (
                    "b/b.go".to_string(),
                    "go".to_string(),
                    "oid-b/b.go".to_string(),
                ),
                (
                    "a/a.go".to_string(),
                    "go".to_string(),
                    "oid-a/a.go".to_string(),
                ),
            ],
            reader_root: "/fresh".to_string(),
            skipped_langs: BTreeSet::new(),
        };

        let (graph, _) = ingest_with_reuse(
            Vec::<Record>::new(),
            &IngestOptions {
                blacklist: &[],
                language: "go",
                config: None,
            },
            Some(&reuse),
        );
        // All nodes landed and the cross-file call survived (it would be lost if
        // edges were merged per-unit before every node existed).
        assert!(
            graph.nodes.contains_key("/fresh/b/b.go"),
            "{:?}",
            graph.nodes
        );
        assert!(graph.nodes.contains_key("/fresh/a/a.go"));
        assert!(graph.nodes.contains_key("scratch/b.Later"));
        assert!(graph.nodes.contains_key("scratch/a.Leaf"));
        assert!(
            graph
                .calls
                .contains(&("scratch/b.Later".to_string(), "scratch/a.Leaf".to_string())),
            "the cached cross-file call must survive unit order: {:?}",
            graph.calls
        );
        assert!(
            graph
                .contains
                .contains(&("scratch".to_string(), "/fresh/b/b.go".to_string()))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// feedback-102: a language whose frontend was skipped re-emits nothing, so
    /// its global module scaffolding (pure-intermediate modules, file-less
    /// descendants, and every `Module -> Module` edge) must be replayed from the
    /// store into the assembled graph — the `graph.jsonl` export source — or the
    /// export is structurally incomplete even though the spliced DB keeps the
    /// seed's rows. The replay is gated on the exact `skipped_langs` verdict, so
    /// a spawned language is never shadowed by stale cache rows.
    #[test]
    fn skipped_language_scaffolding_is_replayed_into_the_assembly() {
        use crate::cache::{CacheKey, FactStore, FileFragment, ModuleScaffolding, ScanConfigKey};
        use crate::graph::{Graph, Location, Node, NodeKind};
        use std::path::PathBuf;

        let dir = std::env::temp_dir().join(format!("apg-splice-scaffold-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let cache_key = CacheKey::compute(&ScanConfigKey::default());
        let mut store = FactStore::at(dir.join("facts"));

        let module = || Node {
            kind: NodeKind::Module,
            ..Node::default()
        };
        let located = |kind: NodeKind, path: &str| Node {
            kind,
            location: Some(Location {
                path: PathBuf::from(path),
                start: 0,
                end: 1,
                start_line: 1,
                end_line: 1,
            }),
            code_type: "src".into(),
            ..Node::default()
        };
        let skipped = "/x/csharp/T.cs";
        let mut prev = Graph::default();
        prev.nodes.insert("Apg".into(), module());
        prev.nodes.insert("Apg.CsharpFrontend".into(), module());
        prev.nodes
            .insert("Apg.CsharpFrontend.Tests".into(), module());
        prev.nodes
            .insert("Apg.CsharpFrontend.Tests.Inline".into(), module());
        prev.nodes
            .insert(skipped.into(), located(NodeKind::File, skipped));
        prev.nodes.insert(
            "Apg.CsharpFrontend.Tests.Program".into(),
            located(NodeKind::Struct, skipped),
        );
        prev.contains
            .insert(("Apg".into(), "Apg.CsharpFrontend".into()));
        prev.contains.insert((
            "Apg.CsharpFrontend".into(),
            "Apg.CsharpFrontend.Tests".into(),
        ));
        prev.contains.insert((
            "Apg.CsharpFrontend.Tests".into(),
            "Apg.CsharpFrontend.Tests.Inline".into(),
        ));
        prev.contains
            .insert(("Apg.CsharpFrontend.Tests".into(), skipped.into()));
        prev.contains
            .insert((skipped.into(), "Apg.CsharpFrontend.Tests.Program".into()));

        let frag = FileFragment::from_graph(&prev, skipped, "csharp/T.cs", "oid-t", "csharp");
        store.put(&frag, "/x", &cache_key).unwrap();
        let scaffolding = ModuleScaffolding::extract(&prev, std::path::Path::new("/x"));
        store.put_scaffolding_all(&scaffolding, &cache_key).unwrap();

        // The whole assembly comes from the cache (the changed language
        // contributes no records in this unit test).
        let reuse = Reuse {
            store: &store,
            cache_key: &cache_key,
            files: vec![("csharp/T.cs".into(), "csharp".into(), "oid-t".into())],
            reader_root: "/x".into(),
            skipped_langs: ["csharp".into()].into_iter().collect(),
        };
        let (graph, _) = ingest_with_reuse(
            Vec::<Record>::new(),
            &IngestOptions {
                blacklist: &[],
                language: "csharp",
                config: None,
            },
            Some(&reuse),
        );
        for m in [
            "Apg",
            "Apg.CsharpFrontend",
            "Apg.CsharpFrontend.Tests",
            "Apg.CsharpFrontend.Tests.Inline",
        ] {
            assert!(
                graph.nodes.contains_key(m),
                "the skipped language's module `{m}` must be replayed: {:?}",
                graph.nodes.keys().collect::<Vec<_>>()
            );
        }
        for (from, to) in [
            ("Apg", "Apg.CsharpFrontend"),
            ("Apg.CsharpFrontend", "Apg.CsharpFrontend.Tests"),
            (
                "Apg.CsharpFrontend.Tests",
                "Apg.CsharpFrontend.Tests.Inline",
            ),
        ] {
            assert!(
                graph.contains.contains(&(from.to_string(), to.to_string())),
                "the `Module -> Module` edge {from} -> {to} must be replayed"
            );
        }
        // The reused file's own unit and its Module→File edge survive.
        assert!(graph.nodes.contains_key(skipped));
        assert!(
            graph
                .contains
                .contains(&("Apg.CsharpFrontend.Tests".to_string(), skipped.to_string()))
        );

        // The replay is gated on the skipped-language verdict: with an empty
        // `skipped_langs` the same store contributes no scaffolding.
        let not_skipped = Reuse {
            store: &store,
            cache_key: &cache_key,
            files: vec![("csharp/T.cs".into(), "csharp".into(), "oid-t".into())],
            reader_root: "/x".into(),
            skipped_langs: BTreeSet::new(),
        };
        let (bare, _) = ingest_with_reuse(
            Vec::<Record>::new(),
            &IngestOptions {
                blacklist: &[],
                language: "csharp",
                config: None,
            },
            Some(&not_skipped),
        );
        assert!(
            !bare.nodes.contains_key("Apg"),
            "a language that was not skipped must not replay cached scaffolding"
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
