//! Two-pass ingestion of unified-schema records into a [`Graph`], including the
//! canonical FQN renderer (SPEC §4).
//!
//! Pass 1 buffers node records and renders canonical FQNs (module
//! `<language-id>.<module-identity>` — the frontend emits the identity verbatim
//! and the ingestor roots it under the `lang_switch` id, PHASE_09 — struct
//! `parent.name`, function `parent.name` / `parent.name(T1,T2)`, Go
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
use std::path::{Path, PathBuf};

use crate::classify::{ApgConfig, classify_code_type};
use crate::graph::{Graph, Location, Node, NodeKind};
use crate::schema::{Record, SCAN_HEAD};

pub struct IngestOptions<'a> {
    pub blacklist: &'a [String],
    pub language: &'a str,
    pub config: Option<&'a ApgConfig>,
    /// The checkout-independent identity base: the git toplevel (or the scan
    /// root when the scanned tree is not a git repository). Every scanner path
    /// is rendered repo-relative against it. `None` (the pure in-memory callers)
    /// is the pass-through sentinel — paths are rendered verbatim.
    pub base: Option<&'a Path>,
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

/// True when the `apg/config.json` structural scope EXCLUDES the repo-relative
/// `identity`: a non-empty `include` list requires a match, and any `exclude`
/// match wins — the same rule the structural scanner's `in_scope` applies to
/// its walk. An absent scope (or no config) is the default ON: nothing is
/// excluded.
fn structural_scope_excludes(identity: &str, config: Option<&ApgConfig>) -> bool {
    let Some(scope) = config.and_then(|c| c.structural.as_ref()) else {
        return false;
    };
    if !scope.include.is_empty()
        && !scope
            .include
            .iter()
            .any(|g| crate::classify::matches_glob(g, identity))
    {
        return true;
    }
    scope
        .exclude
        .iter()
        .any(|g| crate::classify::matches_glob(g, identity))
}

/// The scan-hygiene predicate: a record is out of scope when
///
/// - its canonical FQN carries a user blacklist prefix, or
/// - its repo-relative source path (or identity) sits under a default
///   build-output tree — `.git/**` or `target/**`, via the shared
///   [`crate::classify::is_build_output_path`] predicate, or
/// - that path is gitignored by the containing checkout: the scan's content
///   identity already excludes ignored content, so keeping it would make the
///   graph a function of checkout state rather than of authored content.
///
/// A record on a STRUCTURAL stream (see
/// [`crate::classify::is_structural_language`]) is additionally dropped when the
/// config scope excludes it, so the scanner's claim walk and this blacklist
/// agree exactly on `target`/`.git`/gitignored/config-excluded paths and a
/// structural record is never silently kept or lost. Code streams are
/// unaffected.
///
/// `path` is `None` for records that carry no source location (modules) and
/// for edge endpoints, whose FQN prefix is still honoured. An edge into a
/// dropped record dangles and is pruned by the final cleanup.
fn is_blacklisted(fqn: &str, path: Option<&str>, language: &str, opts: &IngestOptions) -> bool {
    if opts.blacklist.iter().any(|p| fqn.starts_with(p.as_str())) {
        return true;
    }
    let Some(path) = path else {
        return false;
    };
    if crate::classify::is_build_output_path(path) {
        return true;
    }
    if crate::classify::is_structural_language(language)
        && structural_scope_excludes(path, opts.config)
    {
        return true;
    }
    opts.base
        .is_some_and(|base| crate::git::path_is_ignored(base, Path::new(path)))
}

fn file_basename(file: &str) -> String {
    std::path::Path::new(file)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| file.to_string())
}

/// Renders a scanner path as its checkout-independent identity relative to
/// `base` (the git toplevel, or the scan root when the scanned tree is not a
/// git repository): a `/`-separated path with no leading separator, no `.`
/// component and no `..` segment. An already-relative identity (a frontend's
/// dotted package identity, e.g. `pkg.sub` or `@co/ui.src`) passes through
/// unchanged; an absolute path under `base` is stripped to its tail.
///
/// A path outside `base`, or one whose tail would escape `base` with `..`,
/// falls back to its file name so an identity can never embed a checkout
/// component (a leading `/` or a `..` segment is a violation —
/// `requirements.constraint.file-identity-is-repo-relative`).
///
/// An empty `base` is the pure pass-through sentinel the in-memory renderer
/// tests use: the input is returned verbatim.
pub fn repo_relative_identity(base: &Path, path: &str) -> String {
    if base.as_os_str().is_empty() {
        return path.to_string();
    }
    let fallback = || -> String {
        let name = Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();
        if name.is_empty() {
            path.replace('\\', "/").trim_start_matches('/').to_string()
        } else {
            name
        }
    };
    let p = Path::new(path);
    let candidate = if p.is_absolute() {
        match p.strip_prefix(base) {
            Ok(rel) => rel.to_path_buf(),
            Err(_) => return fallback(),
        }
    } else {
        p.to_path_buf()
    };
    let mut out: Vec<String> = Vec::new();
    for comp in candidate.components() {
        match comp {
            std::path::Component::Normal(s) => out.push(s.to_string_lossy().replace('\\', "/")),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if out.pop().is_none() {
                    return fallback();
                }
            }
            std::path::Component::RootDir | std::path::Component::Prefix(_) => {}
        }
    }
    if out.is_empty() {
        return fallback();
    }
    out.join("/")
}

/// Renders a module's canonical FQN by rooting its frontend-emitted dotted
/// identity under the `lang_switch` language id (PHASE_09 language rooting)
/// after rendering the identity repo-relative against `base` (a Markdown
/// module identity is an absolute directory path; every other frontend emits a
/// relative identity, which passes through unchanged):
///
/// ```text
/// root_module_fqn("rust", "apg.ingest", base) -> "rust.apg.ingest"
/// root_module_fqn("py",   "pkg.sub",    base) -> "py.pkg.sub"
/// root_module_fqn("ts",   "@co/ui.src", base) -> "ts.@co/ui.src"
/// root_module_fqn("md",   "/abs/docs",  base) -> "md.docs"
/// ```
///
/// The frontends emit the module identity VERBATIM and UNROOTED (a
/// frontend-baked root would double-root once the ingestor applies it); the
/// ingestor applies the `lang_switch` id here. Rooting is what makes two
/// languages that both define a module identity `apg` distinct (`rust.apg` vs
/// `py.apg`), so a cross-language module-module FQN collision is impossible by
/// construction. Every declaration's scope parent begins with a module identity,
/// so `parent.name` inherits the root without a second transform.
pub fn root_module_fqn(language: &str, identity: &str, base: &Path) -> String {
    format!("{language}.{}", repo_relative_identity(base, identity))
}

/// Applies [`root_module_fqn`] to a declaration's scope parent (or a module
/// identity), leaving an EMPTY parent empty — a declaration with no scope is a
/// scanner anomaly and must not become the bare `<language>.` root.
fn rooted_scope(language: &str, parent: &str, base: &Path) -> String {
    if parent.is_empty() {
        String::new()
    } else {
        root_module_fqn(language, parent, base)
    }
}

/// Roots a `contains` endpoint iff it is a known module identity (the frontends
/// emit `Module -> Module` containment with the raw identity endpoints, while
/// declaration containment uses opaque ids this must never rewrite). Module
/// records for both endpoints are emitted before their containment edge (the
/// frontends emit global module scaffolding first), so membership is exact.
fn root_module_endpoint(
    language: &str,
    endpoint: &str,
    module_identities: &HashMap<String, HashSet<String>>,
    base: &Path,
) -> String {
    match module_identities.get(language) {
        Some(ids) if ids.contains(endpoint) => root_module_fqn(language, endpoint, base),
        _ => endpoint.to_string(),
    }
}

/// Roots a scanner-fact edge endpoint that names a **project symbol**, not a
/// module: a cross-stream (`--targets`) edge carries the target's canonical FQN
/// instead of an opaque id (phase-02 task-9), and the frontend emits that FQN
/// UNROOTED. The endpoint is rooted by its longest module-identity prefix
/// (walked right-to-left over `.`/`/` boundaries, so a nested module wins over
/// its parent); an opaque id or a foreign name has no such prefix and is left
/// unchanged. Only the CURRENT stream's language identities are consulted, so
/// two languages that both define `apg` can never cross.
fn root_edge_endpoint(
    language: &str,
    endpoint: &str,
    module_identities: &HashMap<String, HashSet<String>>,
    base: &Path,
) -> String {
    let Some(identities) = module_identities.get(language) else {
        return endpoint.to_string();
    };
    if identities.contains(endpoint) {
        return root_module_fqn(language, endpoint, base);
    }
    let bytes = endpoint.as_bytes();
    let mut sep = bytes.len();
    while sep > 0 {
        sep -= 1;
        if (bytes[sep] == b'.' || bytes[sep] == b'/')
            && sep > 0
            && identities.contains(&endpoint[..sep])
        {
            return root_module_fqn(language, endpoint, base);
        }
    }
    endpoint.to_string()
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
/// PHASE_09 language rooting: each `FuncDecl.parent` is already the ROOTED
/// scope FQN (the caller applies [`root_module_fqn`] to the frontend's raw
/// parent before buffering — see `ingest_records`), so `parent.name` inherits
/// the language root for every declaration whose parent is a module (e.g.
/// `rust.apg.ingest.foo`). The suffix/shape rules below are unchanged.
///
/// Declarations are grouped by `(parent, name)`: a singleton group renders
/// `parent.name`, an overloaded group renders `parent.name(T1,T2,...)` for every
/// member. Go `init` functions carry no signature, so each is rendered
/// `parent.init#<file-basename>` instead. The per-declaration language drives
/// the Go `init` special case (multi-language scans mix languages in one
/// buffer).
///
/// The `py` stream additionally needs same-scope duplicate-name disambiguation
/// (Python `@overload` stubs whose annotations erase identically, and a
/// conditional redefinition): within a colliding subgroup — members erasing to
/// the SAME param list, including the both-empty `()` case — every member
/// renders the full form `parent.name(T1,T2,...)#<file-basename>:<start_line>`,
/// retaining the erased param-list suffix. Every non-py stream is unchanged.
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
        } else if idxs.iter().all(|&i| decls[i].language == "py") {
            // Python same-scope duplicate-name rule, scoped to the `py` stream
            // (every non-py stream keeps the existing overload shape). Bucket
            // the group by erased param list: a subgroup of more than one
            // member cannot share `parent.name(T1,T2,...)`, so each colliding
            // member renders the full form with its retained param-list suffix
            // plus the `#<file-basename>:<start_line>` disambiguator. A member
            // whose erased param list is unique in the group keeps the bare
            // overload form.
            let mut buckets: Vec<(String, Vec<usize>)> = Vec::new();
            for &i in &idxs {
                let key = decls[i].params.join(",");
                match buckets.iter_mut().find(|(k, _)| *k == key) {
                    Some((_, members)) => members.push(i),
                    None => buckets.push((key, vec![i])),
                }
            }
            for (params, members) in buckets {
                if members.len() > 1 {
                    for i in members {
                        let d = &decls[i];
                        out.push((
                            d.id.clone(),
                            format!(
                                "{parent}.{name}({params})#{}:{}",
                                file_basename(&d.file),
                                d.start_line
                            ),
                        ));
                    }
                } else {
                    let d = &decls[members[0]];
                    out.push((
                        d.id.clone(),
                        format!("{parent}.{name}({})", d.params.join(",")),
                    ));
                }
            }
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
///
/// Win-C hand-off (phase-03 task-6): the returned graph IS the splice delta
/// source. The caller builds `splice::SpliceDelta { graph, targets,
/// removed_fqns, scan }` from it — `targets` being the phase-2 re-emission
/// target set (the delete scope) and `removed_fqns` the delta's removed
/// declarations — and the splicer derives its DML from exactly that. The
/// previous `graph.jsonl` is never re-read and no second assembly runs; the
/// full-record path ([`ingest`]) is unchanged.
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
        // PHASE_09: the skipped language's own `Language` root and its
        // `Language -> Module` edges are global scaffolding too — without them
        // the assembled graph would lack the language root a full rebuild has.
        for l in &scaffolding.languages {
            graph.nodes.entry(l.clone()).or_insert_with(|| Node {
                kind: NodeKind::Language,
                ..Node::default()
            });
        }
        for (from, to) in &scaffolding.language_edges {
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

    // Pass 4 (phase-04 task-16): re-resolve. Pass 1 replaced an UnresolvedTarget
    // placeholder with a cached real node, but a spool-authored (or cached)
    // unresolved edge still names that FQN, so the assembly would carry an
    // unresolved edge to a real symbol — a shape a full scan never produces.
    // Now that EVERY node is merged, convert those edges and drop the
    // UnresolvedTarget rows no unresolved edge still references.
    resolve_unresolved_edges(graph);
}

/// Converts unresolved edges whose target FQN resolves to a real project node in
/// the assembled graph (phase-04 task-16), then GCs the `UnresolvedTarget` rows
/// no remaining unresolved edge references.
///
/// A spool-authored `unresolved_call`/`unresolved_use` to an FQN a cached unit
/// declares as a real `Function`/`Struct` must become the `calls`/`uses` edge a
/// full-scan assembly has. The move keeps `target_type` on the edges that stay
/// unresolved; a resolved `calls`/`uses` edge carries none (a full scan's
/// resolved edges do not). The reference GC mirrors the DB splicer's rule — a
/// shared row lives only while some unresolved edge names it.
fn resolve_unresolved_edges(graph: &mut Graph) {
    let is_fn = |g: &Graph, fqn: &str| {
        g.nodes
            .get(fqn)
            .is_some_and(|n| n.kind == NodeKind::Function)
    };
    let is_struct =
        |g: &Graph, fqn: &str| g.nodes.get(fqn).is_some_and(|n| n.kind == NodeKind::Struct);

    let resolved_calls: Vec<(String, String)> = graph
        .unresolved_calls
        .iter()
        .filter(|(from, to, _)| is_fn(graph, from) && is_fn(graph, to))
        .map(|(from, to, _)| (from.clone(), to.clone()))
        .collect();
    for (from, to) in resolved_calls {
        graph
            .unresolved_calls
            .retain(|(f, t, _)| !(f == &from && t == &to));
        graph.calls.insert((from, to));
    }

    let resolved_uses: Vec<(String, String)> = graph
        .unresolved_uses
        .iter()
        .filter(|(from, to)| (is_fn(graph, from) || is_struct(graph, from)) && is_struct(graph, to))
        .map(|(from, to)| (from.clone(), to.clone()))
        .collect();
    for (from, to) in resolved_uses {
        graph.unresolved_uses.remove(&(from.clone(), to.clone()));
        graph.uses.insert((from, to));
    }

    let referenced: HashSet<String> = graph
        .unresolved_calls
        .iter()
        .map(|(_, to, _)| to.clone())
        .chain(graph.unresolved_uses.iter().map(|(_, to)| to.clone()))
        .collect();
    graph
        .nodes
        .retain(|fqn, node| node.kind != NodeKind::UnresolvedTarget || referenced.contains(fqn));
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
    // File nodes keyed by repo-relative path -> their parent module FQN.
    let mut files: HashMap<String, String> = HashMap::new();
    // Modules are buffered (not claimed/inserted immediately): a package and a
    // type may legally share a name in the JVM (`pkg.A` the package and
    // `pkg.A` the class, e.g. NetBeans' QA test-data project layout), and flat
    // FQN space can't represent both. Modules are inserted only after every
    // struct and function FQN is claimed, so a colliding module yields to the
    // type.
    let mut modules: Vec<String> = Vec::new();
    // The raw (unrooted) module identities seen in the stream, PER LANGUAGE, so
    // a `contains` endpoint can be told from an opaque declaration id and a
    // cross-stream canonical FQN can be rooted by its module prefix.
    let mut module_identities: HashMap<String, HashSet<String>> = HashMap::new();
    // Rooted module FQN -> its language, for the `Language -Contains-> Module`
    // attachment after the module nodes land.
    let mut module_language: HashMap<String, String> = HashMap::new();
    // Every language stream seen, in first-seen order (one Language node each).
    let mut languages: Vec<String> = Vec::new();
    // Current language: starts at the scan's language and switches when a
    // `lang_switch` record (injected by `apg scan` between frontend streams of
    // a multi-language scan) appears. Drives code_type classification and FQN
    // rendering per record.
    let mut lang: String = opts.language.to_string();
    // The checkout-independent identity base (git toplevel / scan-root
    // fallback). `None` is the pass-through sentinel the pure in-memory callers
    // use, so a `""` base renders every path verbatim.
    let base: &Path = opts.base.unwrap_or_else(|| Path::new(""));

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
                    // PHASE_09 language rooting: the frontend emits the module
                    // identity verbatim; the ingestor renders it repo-relative
                    // (a Markdown identity is an absolute directory path) and
                    // roots it under the stream's `lang_switch` id.
                    let rooted = root_module_fqn(&lang, &fqn, base);
                    if is_blacklisted(&rooted, None, &lang, opts) {
                        skipped += 1;
                        continue;
                    }
                    module_identities
                        .entry(lang.clone())
                        .or_default()
                        .insert(fqn.clone());
                    module_language.insert(rooted.clone(), lang.clone());
                    if !languages.contains(&lang) {
                        languages.push(lang.clone());
                    }
                    if !modules.contains(&rooted) {
                        modules.push(rooted);
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
                    let fqn = format!("{}.{name}", rooted_scope(&lang, &parent, base));
                    claim(&mut seen, &id, &fqn, NodeKind::Struct);
                    id_to_fqn.insert(id.clone(), fqn.clone());
                    let identity = repo_relative_identity(base, &path);
                    if is_blacklisted(&fqn, Some(&identity), &lang, opts) {
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
                                path: PathBuf::from(&identity),
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
                    parent: rooted_scope(&lang, &parent, base),
                    name,
                    params,
                    file,
                    path: repo_relative_identity(base, &path),
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
                    // the file and everything in it is out of scope too. The
                    // parent module FQN is rooted (PHASE_09), and the file's
                    // canonical identity is its repo-relative path.
                    let parent = rooted_scope(&lang, &parent, base);
                    let identity = repo_relative_identity(base, &path);
                    if is_blacklisted(&parent, Some(&identity), &lang, opts) {
                        skipped += 1;
                        continue;
                    }
                    if !files.contains_key(&identity) {
                        files.insert(identity.clone(), parent.clone());
                        let code_type = classify_code_type(&path, &identity, &lang, opts.config);
                        graph.nodes.insert(
                            identity.clone(),
                            Node {
                                kind: NodeKind::File,
                                location: Some(Location {
                                    path: PathBuf::from(&identity),
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
                // Rooted code-fact edge endpoints (PHASE_09). `Contains`
                // endpoints are module identities (root exactly) or opaque ids
                // (leave). `Calls`/`Uses` endpoints are opaque ids for emitted
                // units or an UNROOTED canonical FQN for a cross-stream target
                // (phase-02 task-9) — root the latter by its module prefix.
                // `Unresolved*` roots only the `from` (the emitted unit); the
                // `to` is a FOREIGN name, never rooted.
                Record::Contains { from, to } => write_edge(
                    &mut sw,
                    Record::Contains {
                        from: root_module_endpoint(&lang, &from, &module_identities, base),
                        to: root_module_endpoint(&lang, &to, &module_identities, base),
                    },
                ),
                Record::Calls { from, to } => write_edge(
                    &mut sw,
                    Record::Calls {
                        from: root_edge_endpoint(&lang, &from, &module_identities, base),
                        to: root_edge_endpoint(&lang, &to, &module_identities, base),
                    },
                ),
                Record::Uses { from, to } => write_edge(
                    &mut sw,
                    Record::Uses {
                        from: root_edge_endpoint(&lang, &from, &module_identities, base),
                        to: root_edge_endpoint(&lang, &to, &module_identities, base),
                    },
                ),
                Record::UnresolvedCall {
                    from,
                    to,
                    target_type,
                } => write_edge(
                    &mut sw,
                    Record::UnresolvedCall {
                        from: root_edge_endpoint(&lang, &from, &module_identities, base),
                        to,
                        target_type,
                    },
                ),
                Record::UnresolvedUse { from, to } => write_edge(
                    &mut sw,
                    Record::UnresolvedUse {
                        from: root_edge_endpoint(&lang, &from, &module_identities, base),
                        to,
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
        if is_blacklisted(&fqn, Some(&f.path), &f.language, opts) {
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

    // Pass B4 (PHASE_09 language rooting): materialise exactly ONE `Language`
    // node per scanned stream (the bare `lang_switch` id) and attach each
    // surviving rooted module to its language via `Language -Contains-> Module`
    // exactly once. A shadowed module (already claimed by a type of the same
    // name) carries no language edge, matching the module node it lost.
    for language in &languages {
        insert_node(
            &mut graph,
            language.clone(),
            Node {
                kind: NodeKind::Language,
                ..Node::default()
            },
        );
    }
    for (module, language) in &module_language {
        if graph
            .nodes
            .get(module)
            .is_some_and(|n| n.kind == NodeKind::Module)
        {
            graph.contains.insert((language.clone(), module.clone()));
        }
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
                    if is_blacklisted(&a, None, &lang, opts)
                        || is_blacklisted(&b, None, &lang, opts)
                    {
                        skipped += 1;
                        continue;
                    }
                    graph.contains.insert((a, b));
                }
                Record::Calls { from, to } => {
                    let a = resolve(&from);
                    let b = resolve(&to);
                    if is_blacklisted(&a, None, &lang, opts)
                        || is_blacklisted(&b, None, &lang, opts)
                    {
                        skipped += 1;
                        continue;
                    }
                    graph.calls.insert((a, b));
                }
                Record::Uses { from, to } => {
                    let a = resolve(&from);
                    let b = resolve(&to);
                    if is_blacklisted(&a, None, &lang, opts)
                        || is_blacklisted(&b, None, &lang, opts)
                    {
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
                    if is_blacklisted(&a, None, &lang, opts) {
                        skipped += 1;
                        continue;
                    }
                    graph.unresolved_calls.insert((a, to, target_type));
                }
                Record::UnresolvedUse { from, to } => {
                    let a = resolve(&from);
                    if is_blacklisted(&a, None, &lang, opts) {
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
                    if is_blacklisted(&a, None, &lang, opts)
                        || is_blacklisted(&b, None, &lang, opts)
                    {
                        skipped += 1;
                        continue;
                    }
                    graph.details.insert((a, b));
                }
                Record::Reviews { from, to } => {
                    let (a, b) = (resolve(&from), resolve(&to));
                    if is_blacklisted(&a, None, &lang, opts)
                        || is_blacklisted(&b, None, &lang, opts)
                    {
                        skipped += 1;
                        continue;
                    }
                    graph.reviews.insert((a, b));
                }
                Record::DependsOn { from, to } => {
                    let (a, b) = (resolve(&from), resolve(&to));
                    if is_blacklisted(&a, None, &lang, opts)
                        || is_blacklisted(&b, None, &lang, opts)
                    {
                        skipped += 1;
                        continue;
                    }
                    graph.depends_on.insert((a, b));
                }
                Record::Gates { from, to } => {
                    let (a, b) = (resolve(&from), resolve(&to));
                    if is_blacklisted(&a, None, &lang, opts)
                        || is_blacklisted(&b, None, &lang, opts)
                    {
                        skipped += 1;
                        continue;
                    }
                    graph.gates.insert((a, b));
                }
                Record::Satisfies { from, to } => {
                    let (a, b) = (resolve(&from), resolve(&to));
                    if is_blacklisted(&a, None, &lang, opts)
                        || is_blacklisted(&b, None, &lang, opts)
                    {
                        skipped += 1;
                        continue;
                    }
                    graph.satisfies.insert((a, b));
                }
                // Spine edges (GraphModel-SPEC.md; PHASE_01).
                Record::Drives { from, to } => {
                    let (a, b) = (resolve(&from), resolve(&to));
                    if is_blacklisted(&a, None, &lang, opts)
                        || is_blacklisted(&b, None, &lang, opts)
                    {
                        skipped += 1;
                        continue;
                    }
                    graph.drives.insert((a, b));
                }
                Record::Represents { from, to } => {
                    let (a, b) = (resolve(&from), resolve(&to));
                    if is_blacklisted(&a, None, &lang, opts)
                        || is_blacklisted(&b, None, &lang, opts)
                    {
                        skipped += 1;
                        continue;
                    }
                    graph.represents.insert((a, b));
                }
                // New-model §3.3 spec edges (apg-projects).
                Record::RealisedBy { from, to } => {
                    let (a, b) = (resolve(&from), resolve(&to));
                    if is_blacklisted(&a, None, &lang, opts)
                        || is_blacklisted(&b, None, &lang, opts)
                    {
                        skipped += 1;
                        continue;
                    }
                    graph.realised_by.insert((a, b));
                }
                Record::SpecImplementedBy { from, to } => {
                    let (a, b) = (resolve(&from), resolve(&to));
                    if is_blacklisted(&a, None, &lang, opts)
                        || is_blacklisted(&b, None, &lang, opts)
                    {
                        skipped += 1;
                        continue;
                    }
                    graph.spec_implemented_by.insert((a, b));
                }
                Record::Publishes { from, to } => {
                    let (a, b) = (resolve(&from), resolve(&to));
                    if is_blacklisted(&a, None, &lang, opts)
                        || is_blacklisted(&b, None, &lang, opts)
                    {
                        skipped += 1;
                        continue;
                    }
                    graph.publishes.insert((a, b));
                }
                Record::Subscribes { from, to } => {
                    let (a, b) = (resolve(&from), resolve(&to));
                    if is_blacklisted(&a, None, &lang, opts)
                        || is_blacklisted(&b, None, &lang, opts)
                    {
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
        (NodeKind::Language, NodeKind::Module)
            | (NodeKind::Module, NodeKind::Module)
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
    use std::path::Path;

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

    /// A Python function declaration with an explicit start line, so
    /// [`render_function_fqns`]'s `py` duplicate-name rule can be exercised
    /// in-memory (the `fd` helper pins `go` and line 1).
    fn fd_py(
        id: &str,
        parent: &str,
        name: &str,
        params: &[&str],
        file: &str,
        line: u32,
    ) -> FuncDecl {
        FuncDecl {
            id: id.to_string(),
            parent: parent.to_string(),
            name: name.to_string(),
            params: params.iter().map(|s| s.to_string()).collect(),
            file: file.to_string(),
            path: file.to_string(),
            start: 0,
            end: 1,
            start_line: line,
            end_line: line,
            language: "py".to_string(),
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

    // -----------------------------------------------------------------------
    // Real-DB oracle readers (phase-03 task-9). Non-#[test] harness helpers, so
    // they live at the `mod tests` root and are shared by the whole module.
    // -----------------------------------------------------------------------

    /// Runs `query` against a DB file opened read-only and returns every row's
    /// cells as strings (empty when the DB cannot be opened/queried).
    fn db_rows(path: &Path, query: &str) -> Vec<Vec<String>> {
        let db = lbug::Database::new(path, lbug::SystemConfig::default().read_only(true))
            .unwrap_or_else(|e| panic!("open {}: {e}", path.display()));
        let conn = lbug::Connection::new(&db).unwrap();
        let rows = conn
            .query(query)
            .map(|r| {
                r.map(|row| row.iter().map(|v| v.to_string()).collect::<Vec<String>>())
                    .collect::<Vec<Vec<String>>>()
            })
            .unwrap_or_default();
        drop(conn);
        drop(db);
        rows
    }

    /// Every table's row count in a DB file opened read-only — the per-label
    /// NODE counts AND the per-rel-type COUNTS in one map (`show_tables()`
    /// enumerates both node and REL tables).
    fn db_table_counts(path: &Path) -> std::collections::BTreeMap<String, i64> {
        let mut out = std::collections::BTreeMap::new();
        let tables = db_rows(path, "CALL show_tables() RETURN name, type");
        for row in tables {
            let table = row.first().cloned().unwrap_or_default();
            let kind = row.get(1).cloned().unwrap_or_default();
            let q = if kind == "REL" {
                format!("MATCH ()-[r:{table}]->() RETURN count(*)")
            } else {
                format!("MATCH (n:{table}) RETURN count(*)")
            };
            let n = db_rows(path, &q)
                .first()
                .and_then(|r| r.first())
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(-1);
            out.insert(table, n);
        }
        out
    }

    /// The `UnresolvedTarget` rows of a DB file as `(fqn, category)`.
    fn db_unresolved_rows(path: &Path) -> std::collections::BTreeSet<(String, String)> {
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
    fn db_scan_row(path: &Path) -> (String, String, String, String) {
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

    /// unit tier -- pure in-memory: no filesystem, database, git or process.
    mod unit {
        use super::*;

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

        /// Phase-08 task-15: Python duplicate-name disambiguation. Two
        /// identically-annotated `@overload` stubs of one name (their erased
        /// param lists collide) and a same-scope conditional redefinition (the
        /// empty param list) each render a DISTINCT FQN in the FULL form
        /// `parent.name(T1,T2,...)#<file-basename>:<start_line>`: the erased
        /// param-list suffix is RETAINED on the colliding member — including the
        /// empty `()` form, never dropped to `parent.name#...` — with the
        /// `#<file-basename>:<start_line>` disambiguator appended. Because every
        /// colliding member claims a unique FQN, a call to each resolves to its
        /// own Function node and `claim` never panics.
        #[test]
        fn py_duplicate_name_declarations_render_distinct_full_fqns() {
            let decls = [
                // Two `@overload` stubs of `f`, both annotated `int` — the
                // erased param list `int` collides.
                fd_py("n1", "pkg.mod", "f", &["int"], "/root/pkg/mod.py", 10),
                fd_py("n2", "pkg.mod", "f", &["int"], "/root/pkg/mod.py", 24),
                // A same-scope conditional redefinition of `g` — both branches
                // erase to the EMPTY param list.
                fd_py("n3", "pkg.mod", "g", &[], "/root/pkg/mod.py", 40),
                fd_py("n4", "pkg.mod", "g", &[], "/root/pkg/mod.py", 52),
            ];
            let m = fqns(&decls);
            assert_eq!(m["n1"], "pkg.mod.f(int)#mod.py:10");
            assert_eq!(m["n2"], "pkg.mod.f(int)#mod.py:24");
            // The `()` form is retained — NOT dropped to `pkg.mod.g#…`.
            assert_eq!(m["n3"], "pkg.mod.g()#mod.py:40");
            assert_eq!(m["n4"], "pkg.mod.g()#mod.py:52");

            // A call edge referencing either id resolves to that declaration's
            // own Function node: the id -> FQN map is injective.
            let unique: HashSet<&String> = m.values().collect();
            assert_eq!(unique.len(), decls.len(), "every FQN is distinct: {m:?}");

            // The distinct FQNs are exactly what keeps `claim` from panicking on
            // a same-kind collision.
            let mut seen: HashMap<String, (String, NodeKind)> = HashMap::new();
            for (id, fqn) in &m {
                claim(&mut seen, id, fqn, NodeKind::Function);
            }
            assert_eq!(seen.len(), decls.len());
        }

        /// Phase-09 task-2 / task-40: `root_module_fqn` roots a frontend's
        /// verbatim dotted identity under the `lang_switch` language id, for
        /// every language — Rust's dotted module path, Python's package chain,
        /// TypeScript's npm-package + dot-path identity, Markdown's absolute
        /// directory path, Go's module path, and Java's package. Rooting is what
        /// makes two languages that both define a module identity `apg` distinct
        /// (`rust.apg` vs `py.apg`).
        ///
        /// fix-module-identity task-12: the identity is rendered repo-relative
        /// against the base first. A Markdown identity is an absolute directory
        /// path, so `/repo/docs` under base `/repo` renders `md.docs`; every
        /// other frontend identity is already relative and passes through.
        #[test]
        fn root_module_fqn_roots_every_language_identity() {
            let base = Path::new("/repo");
            assert_eq!(
                root_module_fqn("rust", "apg.ingest", base),
                "rust.apg.ingest"
            );
            assert_eq!(root_module_fqn("py", "pkg.sub", base), "py.pkg.sub");
            assert_eq!(root_module_fqn("ts", "@co/ui.src", base), "ts.@co/ui.src");
            assert_eq!(root_module_fqn("md", "/repo/docs", base), "md.docs");
            assert_eq!(
                root_module_fqn("go", "github.com/x/y", base),
                "go.github.com/x/y"
            );
            assert_eq!(root_module_fqn("java", "com.foo", base), "java.com.foo");
            // The empty base is the pass-through sentinel.
            assert_eq!(
                root_module_fqn("md", "/abs/docs", Path::new("")),
                "md./abs/docs"
            );
            // An empty parent is left empty — never the bare `<language>.` root.
            assert_eq!(rooted_scope("rust", "", base), "");
            assert_eq!(rooted_scope("rust", "apg", base), "rust.apg");
        }

        /// fix-module-identity task-2: `repo_relative_identity` renders a
        /// scanner path relative to the git-toplevel base (the scan root is the
        /// non-git fallback), `/`-separated, with no leading `/` and no `..`
        /// segment. An already-relative frontend identity passes through; an
        /// absolute path under the base is stripped; a path that would escape
        /// the base falls back to its file name.
        #[test]
        fn repo_relative_identity_is_checkout_independent() {
            let base = Path::new("/repo");
            // Under the base: the tail, no leading slash.
            assert_eq!(
                repo_relative_identity(base, "/repo/src/load.rs"),
                "src/load.rs"
            );
            // The base itself resolves to the bare file name (never empty).
            assert_eq!(repo_relative_identity(base, "/repo"), "repo");
            // A relative identity passes through unchanged.
            assert_eq!(repo_relative_identity(base, "apg.ingest"), "apg.ingest");
            assert_eq!(repo_relative_identity(base, "@co/ui.src"), "@co/ui.src");
            // Lexical normalisation: `.` dropped, `..` resolved.
            assert_eq!(
                repo_relative_identity(base, "/repo/a/./b/../c.rs"),
                "a/c.rs"
            );
            // No `..` can survive: an escaping relative path falls back.
            assert_eq!(repo_relative_identity(base, "../etc/passwd"), "passwd");
            let out = repo_relative_identity(base, "/repo/../etc/passwd");
            assert!(!out.starts_with('/'), "no leading slash: {out}");
            assert!(!out.split('/').any(|c| c == ".."), "no .. segment: {out}");
            // The empty base is the pass-through sentinel.
            assert_eq!(
                repo_relative_identity(Path::new(""), "/abs/a.rs"),
                "/abs/a.rs"
            );
        }

        /// Phase-09 task-9 / task-40: a declaration whose parent is a module
        /// inherits the rooted module FQN through `parent.name` (the renderer
        /// only concatenates, so rooting the parent at record time roots the
        /// symbol). The shape rules — singleton, overload suffix, Go
        /// `init#<file>` — are unchanged.
        #[test]
        fn rooted_module_parent_yields_rooted_symbol_fqns() {
            let decls = [
                fd("n1", "rust.apg.ingest", "run", &[], "/x/a.rs"),
                fd("n2", "py.pkg.sub", "helper", &[], "/x/a.py"),
            ];
            let m = fqns(&decls);
            assert_eq!(m["n1"], "rust.apg.ingest.run");
            assert_eq!(m["n2"], "py.pkg.sub.helper");
            // Go `init` keeps its file disambiguator under the rooted parent.
            let inits = [
                fd("n3", "go.pkg", "init", &[], "/x/a.go"),
                fd("n4", "go.pkg", "init", &[], "/x/b.go"),
            ];
            let mi = fqns(&inits);
            assert_eq!(mi["n3"], "go.pkg.init#a.go");
            assert_eq!(mi["n4"], "go.pkg.init#b.go");
        }

        /// Phase-09 task-40: language rooting removes only the CROSS-language
        /// collision. A same-kind collision WITHIN one language still fails
        /// loudly in `claim` — the rooted FQNs are no different: two
        /// declarations rendering the same rooted FQN panic rather than silently
        /// overwriting.
        #[test]
        #[should_panic(expected = "FQN collision")]
        fn same_kind_claim_still_panics_under_rooting() {
            let mut seen: HashMap<String, (String, NodeKind)> = HashMap::new();
            claim(&mut seen, "n1", "rust.pkg.F", NodeKind::Function);
            claim(&mut seen, "n2", "rust.pkg.F", NodeKind::Function);
        }

        /// apg-0.17.0 phase-01 task-20: the claim guard accepts the structural
        /// scanner's language-rooted identities — the bare per-format roots
        /// (`md.`/`sh.`/…/`misc.`), rooted submodules, and file-rooted
        /// declarations — without a false same-FQN collision. A re-claim by the
        /// same declaration id (a module's FQN is its id) is idempotent; the
        /// genuine same-kind collision still panics (pinned by
        /// `same_kind_claim_still_panics_under_rooting`).
        #[test]
        fn structural_identities_claim_without_false_collision() {
            let mut seen: HashMap<String, (String, NodeKind)> = HashMap::new();
            // The bare per-format roots are distinct module FQNs.
            for root in [
                "md.",
                "sh.",
                "yaml.",
                "json.",
                "toml.",
                "xml.",
                "dockerfile.",
                "makefile.",
                "ini.",
                "misc.",
            ] {
                claim(&mut seen, root, root, NodeKind::Module);
            }
            // A rooted submodule and file-rooted declarations coexist with
            // their stream root.
            claim(&mut seen, "md.docs", "md.docs", NodeKind::Module);
            claim(&mut seen, "n1", "md.docs/guide.md", NodeKind::Struct);
            claim(&mut seen, "n2", "md.AGENTS.md.title", NodeKind::Struct);
            // A re-claim by the same declaration id is idempotent.
            claim(&mut seen, "md.", "md.", NodeKind::Module);
            assert_eq!(seen.len(), 13);
        }

        /// The edge spool round-trips through an IN-MEMORY `Vec<u8>`/`Cursor`, not a
        /// file: the evidence listed it as "writes and re-reads a spool file", but
        /// its body performs no filesystem I/O, so by the law it is unit (the body
        /// wins over the evidence).
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
    }

    /// int tier -- two or more units wired together, pure in-memory (no
    /// filesystem, database, git or process): the identity renderer chain.
    mod int {
        use super::*;

        /// fix-module-identity task-16: the single identity base flows through
        /// the whole ingest-side render chain in memory —
        /// [`repo_relative_identity`] → [`root_module_fqn`] → [`rooted_scope`] →
        /// `parent.name` ([`render_function_fqns`]) — so a scanner record set
        /// renders checkout-independent module/File/symbol identities end to end.
        ///
        /// The spooling `ingest`/`ingest_records` entry point is e2e (it opens a
        /// `std::env::temp_dir()` spool file), so the assembly contract this
        /// task owns is exercised here on the pure renderer units it is built
        /// from.
        #[test]
        fn repo_relative_identity_flows_through_the_render_chain() {
            let base = Path::new("/repo");
            // A File's canonical identity is its repo-relative path, never the
            // absolute scanner path.
            assert_eq!(
                repo_relative_identity(base, "/repo/internal/store/store.go"),
                "internal/store/store.go"
            );
            // Its parent module identity is rooted off the same base; a relative
            // frontend identity passes through and is rooted verbatim.
            let module = root_module_fqn("go", "github.com/x/y", base);
            assert_eq!(module, "go.github.com/x/y");
            // A declaration under that module renders `parent.name` off the
            // rooted parent.
            let decls = [fd(
                "n1",
                &module,
                "Open",
                &["string"],
                "/repo/internal/store/store.go",
            )];
            assert_eq!(fqns(&decls)["n1"], "go.github.com/x/y.Open");
            // The same file rendered from two checkouts agrees: a Markdown
            // absolute module identity (`/repo/docs` and `/other/docs`) rebases
            // to the SAME repo-relative identity.
            assert_eq!(root_module_fqn("md", "/repo/docs", base), "md.docs");
            assert_eq!(
                root_module_fqn("md", "/other/docs", Path::new("/other")),
                "md.docs"
            );
            // An empty scope stays empty — never a bare `<language>.` root.
            assert_eq!(rooted_scope("go", "", base), "");
        }
    }

    /// e2e tier -- real I/O: these tests drive `ingest`/`ingest_with_reuse`,
    /// whose `ingest_records` spools to `std::env::temp_dir()` (two of them also
    /// stage real temp dirs of their own). Each is `#[ignore]`d, so a plain
    /// `cargo test` never runs one; the only entry point is the named guard
    /// `cargo test-e2e` (= `cargo test tests::e2e:: -- --ignored`).
    mod e2e {
        use super::*;

        #[test]
        #[ignore = "e2e tier: real I/O (temp spool dir); run via cargo test-e2e"]
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
                    base: None,
                },
            );
        }

        #[test]
        #[ignore = "e2e tier: real I/O (temp spool dir); run via cargo test-e2e"]
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
                    base: None,
                },
            );
            assert_eq!(report.shadowed_modules, 1);
            // PHASE_09: the module/type collision is at the ROOTED FQN — every
            // module is rooted under its `lang_switch` id (`java.`); the class
            // survives with its rooted canonical FQN.
            assert!(graph.nodes.contains_key("java.org.pkg.A"));
            assert_eq!(graph.nodes["java.org.pkg.A"].kind, NodeKind::Struct);
            // The parent package and the package nested under the shadowed name
            // survive; the shadowed package itself is not present.
            assert!(graph.nodes.contains_key("java.org.pkg"));
            assert!(graph.nodes.contains_key("java.org.pkg.A.deep"));
            assert!(graph.nodes.contains_key("java.org.pkg.A.deep.B"));
            // Files survive (their FQNs are absolute paths, never rooted) with
            // their own module·file·unit containment chains.
            assert!(graph.nodes.contains_key("/x/A.java"));
            assert!(graph.nodes.contains_key("/y/B.java"));
            assert_eq!(graph.nodes["/x/A.java"].kind, NodeKind::File);
            assert!(
                graph
                    .contains
                    .contains(&("java.org.pkg".to_string(), "/x/A.java".to_string()))
            );
            assert!(
                graph
                    .contains
                    .contains(&("/x/A.java".to_string(), "java.org.pkg.A".to_string()))
            );
            assert!(
                graph
                    .contains
                    .contains(&("java.org.pkg.A.deep".to_string(), "/y/B.java".to_string()))
            );
            assert!(
                graph
                    .contains
                    .contains(&("/y/B.java".to_string(), "java.org.pkg.A.deep.B".to_string()))
            );
            // But the shadowed package is not a parent: its Module→File edge and the
            // package chain through it are pruned.
            assert!(
                !graph
                    .contains
                    .contains(&("java.org.pkg.A".to_string(), "/x/A.java".to_string()))
            );
            assert!(!graph.contains.contains(&(
                "java.org.pkg.A".to_string(),
                "java.org.pkg.A.deep".to_string()
            )));
        }

        #[test]
        #[ignore = "e2e tier: real I/O (temp spool dir); run via cargo test-e2e"]
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
                    base: None,
                },
            );
            // The struct `p.A.test` (from the shadowed package) wins over the
            // method `p.A.test`; the distinct method `p.A.other` survives.
            // PHASE_09: rooting changes only the CROSS-language case — the
            // within-language shadow counts are unchanged and the collision is at
            // the rooted FQN (`java.`).
            assert_eq!(report.shadowed_functions, 1);
            assert_eq!(report.shadowed_modules, 1);
            assert!(graph.nodes.contains_key("java.p.A.test"));
            assert_eq!(graph.nodes["java.p.A.test"].kind, NodeKind::Struct);
            assert!(graph.nodes.contains_key("java.p.A.other"));
            // The shadowed module is gone as a module — `java.p.A` exists only as
            // the winning struct — and the file in it survives but loses its
            // module parent chain (`java.p→java.p.A` module edge pruned).
            assert_eq!(graph.nodes["java.p.A"].kind, NodeKind::Struct);
            assert!(graph.nodes.contains_key("/x/A.java"));
            assert!(graph.nodes.contains_key("/y/test.java"));
            assert!(
                graph
                    .contains
                    .contains(&("java.p".to_string(), "/x/A.java".to_string()))
            );
            assert!(
                !graph
                    .contains
                    .contains(&("java.p".to_string(), "java.p.A".to_string()))
            );
            assert!(
                graph
                    .contains
                    .contains(&("/x/A.java".to_string(), "java.p.A".to_string()))
            );
            assert!(
                graph
                    .contains
                    .contains(&("/y/test.java".to_string(), "java.p.A.test".to_string()))
            );
            // The dropped function's containment (by struct and by file) is pruned;
            // the surviving function's edges stay.
            assert!(
                !graph
                    .contains
                    .contains(&("java.p.A".to_string(), "java.p.A.test".to_string()))
            );
            assert!(
                !graph
                    .contains
                    .contains(&("/x/A.java".to_string(), "java.p.A.test".to_string()))
            );
            assert!(
                graph
                    .contains
                    .contains(&("java.p.A".to_string(), "java.p.A.other".to_string()))
            );
            assert!(
                graph
                    .contains
                    .contains(&("/x/A.java".to_string(), "java.p.A.other".to_string()))
            );
        }

        #[test]
        #[ignore = "e2e tier: real I/O (temp spool dir); run via cargo test-e2e"]
        fn module_replaces_unresolved_target() {
            // PHASE_09: rooting retires the original 'replaces' premise. A module's
            // identity is rooted (`java.tests`) while a foreign/unresolved name
            // stays verbatim (`tests`), so the module and the unresolved placeholder
            // no longer share an FQN — both survive, each with its own kind. (The
            // original behaviour, a real declaration superseding an
            // UnresolvedTarget placeholder at the SAME FQN, is still exercised by
            // `reuse_splice_reresolves_unresolved_edges_to_cached_real_nodes`.)
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
                    base: None,
                },
            );
            // The module is rooted; the unresolved name is not.
            assert!(graph.nodes.contains_key("java.tests"));
            assert_eq!(graph.nodes["java.tests"].kind, NodeKind::Module);
            assert!(graph.nodes.contains_key("tests"));
            assert_eq!(graph.nodes["tests"].kind, NodeKind::UnresolvedTarget);
        }

        #[test]
        #[ignore = "e2e tier: real I/O (temp spool dir); run via cargo test-e2e"]
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
                // Two further languages reuse the SAME module identity `apg`:
                // rooting keeps them distinct (`rust.apg` vs `py.apg`).
                Record::LangSwitch {
                    language: "rust".to_string(),
                },
                Record::Module {
                    fqn: "apg".to_string(),
                },
                Record::LangSwitch {
                    language: "py".to_string(),
                },
                Record::Module {
                    fqn: "apg".to_string(),
                },
            ];
            let (graph, report) = ingest(
                records,
                &IngestOptions {
                    blacklist: &[],
                    language: "go",
                    config: None,
                    base: None,
                },
            );
            assert_eq!(report.skipped, 0);
            // PHASE_09: rooting makes a cross-language module-module FQN
            // collision impossible by construction, so nothing is shadowed.
            assert_eq!(
                report.shadowed_modules, 0,
                "rooting must keep rust.apg and py.apg distinct"
            );
            assert_eq!(report.shadowed_functions, 0);
            // Rooted module FQNs, one per language.
            for m in ["go.github.com/x/y", "ts.@co/ui", "rust.apg", "py.apg"] {
                assert_eq!(
                    graph.nodes.get(m).map(|n| n.kind),
                    Some(NodeKind::Module),
                    "module `{m}` must be rooted under its language"
                );
            }
            // One Language node per `lang_switch` stream, bare id.
            for l in ["go", "ts", "rust", "py"] {
                assert_eq!(
                    graph.nodes.get(l).map(|n| n.kind),
                    Some(NodeKind::Language),
                    "language root `{l}` must be materialised"
                );
            }
            // `Language -Contains-> Module` exactly once per module.
            for (lang, m) in [
                ("go", "go.github.com/x/y"),
                ("ts", "ts.@co/ui"),
                ("rust", "rust.apg"),
                ("py", "py.apg"),
            ] {
                assert!(
                    graph.contains.contains(&(lang.to_string(), m.to_string())),
                    "Language `{lang}` must contain `{m}`"
                );
            }
            // Rooted symbols inherit through `parent.name`; Go init is
            // file-disambiguated, the TS function named `init` is not.
            assert!(graph.nodes.contains_key("go.github.com/x/y.Store"));
            assert!(graph.nodes.contains_key("go.github.com/x/y.init#store.go"));
            assert!(graph.nodes.contains_key("ts.@co/ui.src.app.App"));
            assert!(graph.nodes.contains_key("ts.@co/ui.src.app.init"));
            // code_type is per-language: the Go store is src, the .test.ts file
            // (ts test rule) and its struct are test. File FQNs are paths.
            assert_eq!(graph.nodes["/abs/store.go"].code_type, "src");
            assert_eq!(graph.nodes["/proj/src/app.ts"].code_type, "src");
            assert_eq!(graph.nodes["/proj/src/app.test.ts"].code_type, "test");
            assert_eq!(graph.nodes["ts.@co/ui.src.app.Helper"].code_type, "test");
            // Module→File containment is rooted too.
            assert!(
                graph
                    .contains
                    .contains(&("go.github.com/x/y".to_string(), "/abs/store.go".to_string())),
                "the rooted module must contain its file"
            );
        }

        #[test]
        #[ignore = "e2e tier: real I/O (temp spool dir); run via cargo test-e2e"]
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
                    base: None,
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
                    base: None,
                },
            );
            let n = &graph.nodes[SCAN_HEAD];
            assert_eq!(n.kind, NodeKind::Scan);
            assert_eq!(n.git_sha, None);
            assert_eq!(n.git_clean, None);
            assert_eq!(n.content_key, None);
        }

        #[test]
        #[ignore = "e2e tier: real I/O (temp spool dir); run via cargo test-e2e"]
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
                    base: None,
                },
            );
            assert_eq!(report.skipped, 0);
            // PHASE_09: every module/symbol FQN is rooted under the scan's
            // `lang_switch` id (`go.`); the unresolved target stays verbatim.
            assert!(graph.nodes.contains_key("go.github.com/x/y.Store"));
            assert!(graph.nodes.contains_key("go.github.com/x/y.Compute"));
            assert!(graph.nodes.contains_key("go.github.com/x/y.Store.Get"));
            assert!(graph.nodes.contains_key("fmt.Errorf"));
            // File layer: module contains the file, the file contains its units,
            // and methods stay under their struct.
            assert!(
                graph
                    .contains
                    .contains(&("go.github.com/x/y".to_string(), "/abs/store.go".to_string()))
            );
            assert!(graph.contains.contains(&(
                "/abs/store.go".to_string(),
                "go.github.com/x/y.Store".to_string()
            )));
            assert!(graph.contains.contains(&(
                "/abs/store.go".to_string(),
                "go.github.com/x/y.Compute".to_string()
            )));
            assert!(!graph.contains.contains(&(
                "go.github.com/x/y".to_string(),
                "go.github.com/x/y.Store".to_string()
            )));
            assert!(graph.contains.contains(&(
                "go.github.com/x/y.Store".to_string(),
                "go.github.com/x/y.Store.Get".to_string()
            )));
            assert!(graph.unresolved_calls.contains(&(
                "go.github.com/x/y.Compute".to_string(),
                "fmt.Errorf".to_string(),
                String::new()
            )));
        }

        #[test]
        #[ignore = "e2e tier: real I/O (temp spool dir); run via cargo test-e2e"]
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
                    base: None,
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
        #[ignore = "e2e tier: real I/O (temp spool dir); run via cargo test-e2e"]
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
                    to: "go.github.com/x/y.Gateway".to_string(),
                },
                Record::PlannedNode {
                    fqn: "go.github.com/x/y.Gateway".to_string(),
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
                    base: None,
                },
            );
            let fqn = "go.github.com/x/y.Gateway".to_string();
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
        #[ignore = "e2e tier: real I/O (temp spool dir); run via cargo test-e2e"]
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
                    blacklist: &["go.drop.mod".to_string()],
                    language: "go",
                    config: None,
                    base: None,
                },
            );
            assert!(report.skipped >= 3);
            // PHASE_09: every module FQN is rooted (`go.`), and
            // `is_blacklisted` matches the ROOTED FQN — hence the rooted pattern.
            assert!(graph.nodes.contains_key("go.keep.mod"));
            assert!(!graph.nodes.contains_key("go.drop.mod"));
            assert!(!graph.nodes.contains_key("go.drop.mod.B"));
            // A file whose parent module is blacklisted is dropped along with its
            // units; the surviving file keeps its module and unit edges.
            assert!(!graph.nodes.contains_key("/x/b.go"));
            assert!(graph.nodes.contains_key("/x/a.go"));
            assert!(
                graph
                    .contains
                    .contains(&("go.keep.mod".to_string(), "/x/a.go".to_string()))
            );
            assert!(
                graph
                    .contains
                    .contains(&("/x/a.go".to_string(), "go.keep.mod.A".to_string()))
            );
            assert!(!graph.contains.is_empty());
        }

        #[test]
        #[ignore = "e2e tier: real I/O (temp spool dir); run via cargo test-e2e"]
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
                    base: None,
                },
                Some(&reuse),
            );
            // All nodes landed and the cross-file call survived (it would be lost if
            // edges were merged per-unit before every node existed).
            assert!(graph.nodes.contains_key("b/b.go"), "{:?}", graph.nodes);
            assert!(graph.nodes.contains_key("a/a.go"));
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
                    .contains(&("scratch".to_string(), "b/b.go".to_string()))
            );
            let _ = std::fs::remove_dir_all(&dir);
        }

        /// Phase-04 task-24: the reuse splice re-resolves unresolved edges whose
        /// target FQN a cached unit declares as a real node. Pass 1 replaces the
        /// UnresolvedTarget placeholder, but the spool-authored unresolved edges
        /// still name the FQN; pass 4 converts them to `calls`/`uses` and GCs the
        /// now-unreferenced row, so the assembled graph equals a full-scan assembly
        /// of the same tree — the falsifiable re-resolution claim (task-16).
        #[test]
        #[ignore = "e2e tier: real I/O (temp spool dir); run via cargo test-e2e"]
        fn reuse_splice_reresolves_unresolved_edges_to_cached_real_nodes() {
            use crate::cache::{CacheKey, FactStore, FileFragment, ScanConfigKey};
            use crate::graph::{Graph, Location, Node, NodeKind};
            use std::path::PathBuf;

            let dir = std::env::temp_dir().join(format!("apg-reresolve-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            let cache_key = CacheKey::compute(&ScanConfigKey::default());
            let mut store = FactStore::at(dir.join("facts"));

            // The cached unit b/b.go declares the two real targets.
            let mut cached = Graph::default();
            cached.nodes.insert(
                "go.scratch".to_string(),
                Node {
                    kind: NodeKind::Module,
                    ..Node::default()
                },
            );
            cached.nodes.insert(
                "b.go".to_string(),
                Node {
                    kind: NodeKind::File,
                    location: Some(Location {
                        path: PathBuf::from("b.go"),
                        start: 0,
                        end: 0,
                        start_line: 1,
                        end_line: 9,
                    }),
                    ..Node::default()
                },
            );
            for (fqn, kind, start_line) in [
                ("go.scratch.Callee", NodeKind::Function, 2u32),
                ("go.scratch.Model", NodeKind::Struct, 6u32),
            ] {
                cached.nodes.insert(
                    fqn.to_string(),
                    Node {
                        kind,
                        location: Some(Location {
                            path: PathBuf::from("b.go"),
                            start: 0,
                            end: 1,
                            start_line,
                            end_line: start_line,
                        }),
                        code_type: "src".into(),
                        ..Node::default()
                    },
                );
            }
            cached
                .contains
                .insert(("go.scratch".to_string(), "b.go".to_string()));
            cached
                .contains
                .insert(("b.go".to_string(), "go.scratch.Callee".to_string()));
            cached
                .contains
                .insert(("b.go".to_string(), "go.scratch.Model".to_string()));
            let frag = FileFragment::from_graph(&cached, "b.go", "b.go", "oid-b", "go");
            store.put(&frag, "/fresh", &cache_key).unwrap();

            // The freshly re-emitted spool: a.go's Caller authors unresolved edges
            // at BOTH cached real FQNs plus one genuinely-unresolved target.
            let spool = vec![
                Record::Module {
                    fqn: "scratch".to_string(),
                },
                Record::Function {
                    id: "s1".to_string(),
                    parent: "scratch".to_string(),
                    name: "Caller".to_string(),
                    params: vec![],
                    file: "a.go".to_string(),
                    path: "a.go".to_string(),
                    start: 0,
                    end: 1,
                    start_line: 1,
                    end_line: 1,
                },
                file_rec("a.go", "scratch", 10),
                Record::Unresolved {
                    fqn: "go.scratch.Callee".to_string(),
                    category: Some("unknown".to_string()),
                },
                Record::Unresolved {
                    fqn: "go.scratch.Model".to_string(),
                    category: Some("external".to_string()),
                },
                Record::Unresolved {
                    fqn: "ghost.External".to_string(),
                    category: Some("external".to_string()),
                },
                Record::UnresolvedCall {
                    from: "s1".to_string(),
                    to: "go.scratch.Callee".to_string(),
                    target_type: "func()".to_string(),
                },
                Record::UnresolvedUse {
                    from: "s1".to_string(),
                    to: "go.scratch.Model".to_string(),
                },
                Record::UnresolvedCall {
                    from: "s1".to_string(),
                    to: "ghost.External".to_string(),
                    target_type: String::new(),
                },
            ];
            let reuse = Reuse {
                store: &store,
                cache_key: &cache_key,
                files: vec![("b.go".to_string(), "go".to_string(), "oid-b".to_string())],
                reader_root: "/fresh".to_string(),
                skipped_langs: BTreeSet::new(),
            };
            let (assembled, _) = ingest_with_reuse(
                spool,
                &IngestOptions {
                    blacklist: &[],
                    language: "go",
                    config: None,
                    base: None,
                },
                Some(&reuse),
            );

            // The same tree resolved from scratch: the cached targets are declared
            // here and the call/use are RESOLVED edges.
            let full = vec![
                Record::Module {
                    fqn: "scratch".to_string(),
                },
                Record::Function {
                    id: "f1".to_string(),
                    parent: "scratch".to_string(),
                    name: "Caller".to_string(),
                    params: vec![],
                    file: "a.go".to_string(),
                    path: "a.go".to_string(),
                    start: 0,
                    end: 1,
                    start_line: 1,
                    end_line: 1,
                },
                Record::Function {
                    id: "f2".to_string(),
                    parent: "scratch".to_string(),
                    name: "Callee".to_string(),
                    params: vec![],
                    file: "b.go".to_string(),
                    path: "b.go".to_string(),
                    start: 0,
                    end: 1,
                    start_line: 2,
                    end_line: 2,
                },
                srec("f3", "scratch", "Model", "b.go"),
                file_rec("a.go", "scratch", 10),
                file_rec("b.go", "scratch", 9),
                Record::Unresolved {
                    fqn: "ghost.External".to_string(),
                    category: Some("external".to_string()),
                },
                Record::Calls {
                    from: "f1".to_string(),
                    to: "f2".to_string(),
                },
                Record::Uses {
                    from: "f1".to_string(),
                    to: "f3".to_string(),
                },
                Record::UnresolvedCall {
                    from: "f1".to_string(),
                    to: "ghost.External".to_string(),
                    target_type: String::new(),
                },
            ];
            let (reference, _) = ingest(
                full,
                &IngestOptions {
                    blacklist: &[],
                    language: "go",
                    config: None,
                    base: None,
                },
            );

            // (a) the converted edges appear in `calls`/`uses`.
            assert!(
                assembled.calls.contains(&(
                    "go.scratch.Caller".to_string(),
                    "go.scratch.Callee".to_string()
                )),
                "the unresolved call to a cached real Function must move to calls: {:?}",
                assembled.calls
            );
            assert!(
                assembled.uses.contains(&(
                    "go.scratch.Caller".to_string(),
                    "go.scratch.Model".to_string()
                )),
                "the unresolved use of a cached real Struct must move to uses: {:?}",
                assembled.uses
            );
            // (b) NO unresolved edge targets a real (non-UnresolvedTarget) node.
            for (from, to, _) in &assembled.unresolved_calls {
                assert!(
                    assembled
                        .nodes
                        .get(to)
                        .is_some_and(|n| n.kind == NodeKind::UnresolvedTarget),
                    "unresolved_call {from} -> {to} must not target a real project FQN"
                );
            }
            for (from, to) in &assembled.unresolved_uses {
                assert!(
                    assembled
                        .nodes
                        .get(to)
                        .is_some_and(|n| n.kind == NodeKind::UnresolvedTarget),
                    "unresolved_use {from} -> {to} must not target a real project FQN"
                );
            }
            // (c) the genuine unresolved edge + exactly ONE UnresolvedTarget row.
            assert!(assembled.unresolved_calls.contains(&(
                "go.scratch.Caller".to_string(),
                "ghost.External".to_string(),
                String::new()
            )));
            let unresolved = |g: &Graph| -> BTreeSet<String> {
                g.nodes
                    .iter()
                    .filter(|(_, n)| n.kind == NodeKind::UnresolvedTarget)
                    .map(|(fqn, _)| fqn.clone())
                    .collect()
            };
            assert_eq!(
                unresolved(&assembled),
                BTreeSet::from(["ghost.External".to_string()]),
                "exactly one shared UnresolvedTarget row may survive"
            );
            // (d) the assembled node/edge/unresolved sets equal a full-scan assembly.
            let node_set = |g: &Graph| -> BTreeSet<String> { g.nodes.keys().cloned().collect() };
            assert_eq!(node_set(&assembled), node_set(&reference));
            assert_eq!(assembled.contains, reference.contains);
            assert_eq!(assembled.calls, reference.calls);
            assert_eq!(assembled.uses, reference.uses);
            assert_eq!(assembled.unresolved_calls, reference.unresolved_calls);
            assert_eq!(assembled.unresolved_uses, reference.unresolved_uses);
            assert_eq!(unresolved(&assembled), unresolved(&reference));

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
        #[ignore = "e2e tier: real I/O (temp spool dir); run via cargo test-e2e"]
        fn skipped_language_scaffolding_is_replayed_into_the_assembly() {
            use crate::cache::{
                CacheKey, FactStore, FileFragment, ModuleScaffolding, ScanConfigKey,
            };
            use crate::graph::{Graph, Location, Node, NodeKind};
            use std::path::PathBuf;

            let dir =
                std::env::temp_dir().join(format!("apg-splice-scaffold-{}", std::process::id()));
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
            let skipped = "csharp/T.cs";
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
                    base: None,
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
                    base: None,
                },
                Some(&not_skipped),
            );
            assert!(
                !bare.nodes.contains_key("Apg"),
                "a language that was not skipped must not replay cached scaffolding"
            );
            let _ = std::fs::remove_dir_all(dir);
        }

        /// Phase-03 task-9 — the splice path END-TO-END: a scratch /tmp git repo
        /// driven by the CANDIDATE binary only
        /// (global.constraint.no-real-project-test). (1) The DETERMINISTIC
        /// no-full-DB-rebuild observable: the distinct splice verdict line is
        /// PRESENT on a localized re-scan and ABSENT on a full rebuild, whose path
        /// emits the full-load lines instead — a named observable, not the mere
        /// absence of a frontend spawn. (2) The ENUMERATED equivalence oracle:
        /// spliced vs full rebuild on per-label and per-rel-type counts, the
        /// UnresolvedTarget set by FQN WITH categories (per-category counts), the
        /// `Scan` row, a spine query, and the EXPORT (`graph.jsonl` equality plus a
        /// JSONL → Graph → JSONL round-trip).
        #[test]
        #[ignore = "e2e tier: real I/O (scratch repo/spawned apg/db.lbug); run via cargo test-e2e"]
        fn splice_path_is_taken_and_equals_a_full_rebuild() {
            use crate::testutil::{ApgCommand, Repo};

            let repo = Repo::new("p3-splice-e2e");
            let home = repo.root.join("home");
            std::fs::create_dir_all(&home).unwrap();
            // A real Go module: `b` depends on `a`, `c` depends on `b`.
            repo.write("go.mod", "module scratch\n\ngo 1.21\n");
            repo.write(
                "a/a.go",
                "package a\n\n// A is a struct.\ntype A struct {\n\tX int\n}\n\n// Leaf is the leaf function.\nfunc Leaf() int { return 1 }\n",
            );
            repo.write(
                "b/b.go",
                "package b\n\nimport \"scratch/a\"\n\n// B is a struct.\ntype B struct {\n\tA a.A\n}\n\n// Foo calls the leaf.\nfunc Foo() int { return a.Leaf() }\n",
            );
            repo.write(
                "c/c.go",
                "package c\n\nimport \"scratch/b\"\n\n// Bar calls Foo.\nfunc Bar() int { return b.Foo() }\n",
            );
            repo.commit_all("source");
            let run = |args: &[&str]| {
                ApgCommand::new(args)
                    .cwd(&repo.root)
                    .env("HOME", home.to_str().unwrap())
                    .output()
            };
            let init = run(&["init", "."]);
            assert!(
                init.status.success(),
                "init: {}",
                String::from_utf8_lossy(&init.stderr)
            );
            repo.commit_all("apg init");

            let trans = repo
                .root
                .join(crate::specs::LAYOUT)
                .join(crate::specs::TRANS);
            let db_path = trans.join("db.lbug");
            let jsonl_path = trans.join("graph.jsonl");

            // The cold scan has no previous DB: a FULL load, with no splice verdict.
            let cold = run(&["scan", "."]);
            let cold_err = String::from_utf8_lossy(&cold.stderr).into_owned();
            assert!(cold.status.success(), "cold scan: {cold_err}");
            assert!(
                cold_err.contains("[load] writing parquet load files"),
                "the cold scan must full-load: {cold_err}"
            );
            assert!(
                !cold_err.contains("[load] splice:"),
                "the cold scan must not splice: {cold_err}"
            );
            // The cold scan's own Scan row — the seed the splice must REFRESH.
            let cold_scan = db_scan_row(&db_path);

            // A localized BODY-ONLY edit to the leaf file: `a.go` changes, b/c do not.
            repo.write(
                "a/a.go",
                "package a\n\n// A is a struct.\ntype A struct {\n\tX int\n}\n\n// Leaf is the leaf function.\nfunc Leaf() int { return 42 }\n",
            );

            let inc = run(&["scan", "."]);
            let inc_err = String::from_utf8_lossy(&inc.stderr).into_owned();
            assert!(inc.status.success(), "incremental scan: {inc_err}");
            // (1) The distinct splice verdict is PRESENT and the full-load lines are
            // ABSENT — the splice path was genuinely taken (a full rebuild cannot
            // produce this line).
            assert!(
                inc_err.contains("[load] splice:"),
                "the localized re-scan must take the splice path: {inc_err}"
            );
            assert!(
                !inc_err.contains("[load] writing parquet load files"),
                "the splice path must skip the full load: {inc_err}"
            );
            assert!(
                inc_err.contains("upserted") && inc_err.contains("full load skipped"),
                "the splice verdict must name the applied delta and the skipped full load: {inc_err}"
            );

            // Snapshot the spliced pair, then FORCE a full rebuild of the SAME tree:
            // clear the DB, the export AND the shared fact cache so neither the
            // fast-path nor win-B reuse can engage.
            let spliced_db = repo.root.join("spliced.lbug");
            let spliced_jsonl = repo.root.join("spliced.jsonl");
            std::fs::copy(&db_path, &spliced_db).unwrap();
            std::fs::copy(&jsonl_path, &spliced_jsonl).unwrap();
            std::fs::remove_file(&db_path).unwrap();
            std::fs::remove_file(&jsonl_path).unwrap();
            let _ = std::fs::remove_dir_all(repo.root.join(".git/apg/facts"));
            let full = run(&["scan", "."]);
            let full_err = String::from_utf8_lossy(&full.stderr).into_owned();
            assert!(full.status.success(), "full rebuild: {full_err}");
            assert!(
                full_err.contains("[load] writing parquet load files"),
                "the rebuild must full-load: {full_err}"
            );
            assert!(
                !full_err.contains("[load] splice:"),
                "a full rebuild must not emit the splice verdict: {full_err}"
            );

            // (2) The enumerated equivalence oracle: spliced vs full rebuild.
            let spliced_counts = db_table_counts(&spliced_db);
            let full_counts = db_table_counts(&db_path);
            assert_eq!(
                spliced_counts, full_counts,
                "every table's count must equal a full rebuild"
            );
            for label in ["Module", "File", "Struct", "Function", "UnresolvedTarget"] {
                assert!(
                    spliced_counts.contains_key(label),
                    "the oracle must enumerate the {label} node table: {spliced_counts:?}"
                );
            }
            for table in [
                "Contains",
                "Calls",
                "Uses",
                "UnresolvedCall",
                "UnresolvedUse",
            ] {
                assert!(
                    spliced_counts.contains_key(table),
                    "the oracle must enumerate the {table} rel table: {spliced_counts:?}"
                );
                assert_eq!(
                    spliced_counts.get(table),
                    full_counts.get(table),
                    "{table} count must equal a full rebuild"
                );
            }

            let spliced_unres = db_unresolved_rows(&spliced_db);
            let full_unres = db_unresolved_rows(&db_path);
            assert_eq!(
                spliced_unres, full_unres,
                "the UnresolvedTarget set by (fqn, category) must equal a full rebuild's"
            );
            let per_category = |rows: &std::collections::BTreeSet<(String, String)>| {
                let mut m: std::collections::BTreeMap<String, usize> =
                    std::collections::BTreeMap::new();
                for (_, c) in rows {
                    *m.entry(c.clone()).or_default() += 1;
                }
                m
            };
            assert_eq!(
                per_category(&spliced_unres),
                per_category(&full_unres),
                "the per-category unresolved counts must equal a full rebuild's"
            );

            // The Scan row: REFRESHED by the delta (never the seeded cold-scan
            // row) and exactly the spliced export's line 1. Two separate scans
            // legitimately carry different `scanned_at`/content keys, so the
            // full-rebuild comparison is on the identity fields (sha/clean), with
            // the line-1 tie asserted per scan.
            let spliced_scan = db_scan_row(&spliced_db);
            assert_ne!(
                spliced_scan, cold_scan,
                "the seeded Scan row must be deleted+reinserted, not preserved"
            );
            assert_ne!(
                spliced_scan.3, cold_scan.3,
                "the refreshed Scan row must carry THIS scan's scanned_at: {spliced_scan:?} vs {cold_scan:?}"
            );
            let spliced_jsonl_text = std::fs::read_to_string(&spliced_jsonl).unwrap();
            let spliced_first = spliced_jsonl_text.lines().next().unwrap();
            let sv: serde_json::Value = serde_json::from_str(spliced_first).unwrap();
            assert_eq!(
                sv["type"], "scan_meta",
                "line 1 leads with scan_meta: {spliced_first}"
            );
            assert_eq!(sv["git_sha"].as_str().unwrap_or_default(), spliced_scan.0);
            assert_eq!(
                sv["git_clean"].as_bool().unwrap_or(false),
                spliced_scan.1 == "true"
            );
            assert_eq!(
                sv["content_key"].as_str().unwrap_or_default(),
                spliced_scan.2
            );
            assert_eq!(
                sv["scanned_at"].as_str().unwrap_or_default(),
                spliced_scan.3
            );

            // The full rebuild's Scan row/line 1, and the identity fields it must
            // share with the spliced one (same tree, same HEAD, same dirty state).
            let full_scan = db_scan_row(&db_path);
            assert_eq!(
                (spliced_scan.0.clone(), spliced_scan.1.clone()),
                (full_scan.0.clone(), full_scan.1.clone()),
                "the spliced Scan row's sha/clean must match a full rebuild's"
            );
            let full_jsonl = std::fs::read_to_string(&jsonl_path).unwrap();
            let full_first = full_jsonl.lines().next().unwrap();
            let fv: serde_json::Value = serde_json::from_str(full_first).unwrap();
            assert_eq!(fv["type"], "scan_meta", "line 1 leads with scan_meta");
            assert_eq!(fv["scanned_at"].as_str().unwrap_or_default(), full_scan.3);
            assert_eq!(fv["content_key"].as_str().unwrap_or_default(), full_scan.2);

            // A sample spine query (a bare scratch repo has no authored nodes, so
            // both sides are empty — the equality is still a real assertion).
            let spine = "MATCH (r:Requirement)-[:Drives]->(:Entity)-[:RealisedBy]->\
                         (:Container)-[:SpecImplementedBy]->(c) RETURN r.fqn, c.fqn";
            assert_eq!(
                db_rows(&spliced_db, spine),
                db_rows(&db_path, spine),
                "the spine query must agree with a full rebuild"
            );

            // The EXPORT: record-set equality with the full rebuild's for every
            // record kind EXCEPT the per-scan `scan_meta` line (each scan's own
            // line 1 is tied to its own Scan row above), plus a JSONL → Graph →
            // JSONL round-trip through the re-ingest reader.
            let canon = |t: &str| -> std::collections::BTreeSet<String> {
                t.lines()
                    .filter(|l| !l.starts_with("{\"type\":\"scan_meta\""))
                    .map(str::to_string)
                    .collect()
            };
            assert_eq!(
                canon(&spliced_jsonl_text),
                canon(&full_jsonl),
                "every non-scan_meta export record must equal a full rebuild's"
            );
            let back = crate::load::tests::read_graph_jsonl(&spliced_jsonl).unwrap();
            let again = repo.root.join("again.jsonl");
            crate::load::write_graph_jsonl(&back, &again).unwrap();
            let again_text = std::fs::read_to_string(&again).unwrap();
            assert_eq!(
                canon(&spliced_jsonl_text),
                canon(&again_text),
                "graph.jsonl must round-trip through read_graph_jsonl"
            );
            // …including its `scan_meta` line 1 (the Scan node round-trips too).
            assert_eq!(
                again_text.lines().next().unwrap(),
                spliced_first,
                "the round-tripped export must reproduce the scan_meta line"
            );

            let _ = std::fs::remove_dir_all(&repo.root);
        }
    }
}
