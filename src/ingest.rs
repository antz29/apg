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
/// for edge endpoints, whose FQN prefix is still honoured. A **structural
/// module** identity is a repo-relative tree in disguise (`misc.apg/.trans`):
/// the scanner's walk prunes only `target`/`node_modules`/`.git`/`.worktrees`
/// and never consults `.gitignore`, so a path-less structural module whose
/// identity sits under a gitignored/build-output tree is dropped here exactly
/// as its File records are — otherwise an empty `module:misc.<ignored-tree>`
/// would survive. The check is scoped to an fqn actually rooted under the
/// current structural language id, so a code FQN or an opaque id is never
/// re-interpreted as a path. An edge into a dropped record dangles and is
/// pruned by the final cleanup.
fn is_blacklisted(fqn: &str, path: Option<&str>, language: &str, opts: &IngestOptions) -> bool {
    if opts.blacklist.iter().any(|p| fqn.starts_with(p.as_str())) {
        return true;
    }
    let Some(path) = path else {
        // The bare `<language>.` root (empty identity) is the repo root: it is
        // never build-output/gitignored, so it always survives.
        if crate::classify::is_structural_language(language)
            && let Some(identity) = fqn
                .strip_prefix(language)
                .and_then(|rest| rest.strip_prefix('.'))
        {
            if crate::classify::is_build_output_path(identity) {
                return true;
            }
            if opts
                .base
                .is_some_and(|base| crate::git::path_is_ignored(base, Path::new(identity)))
            {
                return true;
            }
        }
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
/// A path that resolves to `base` **itself** — the repository root, whose
/// repo-relative identity is empty — renders the empty string, NOT the
/// `file_name(base)` checkout basename: a module rooted at the repo root is
/// the bare `<language>.` root (`md.`), never a checkout-named module
/// (`md.apg`) (`requirements.constraint.no-checkout-named-module`).
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
        // The path resolves to the base itself (or an empty identity): the
        // repo-root identity is empty. Every genuinely escaping/foreign path
        // returned `fallback()` above, so only the under-base/empty case lands
        // here.
        return String::new();
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
/// root_module_fqn("md",   "",           base) -> "md."     // the repo root
/// root_module_fqn("sh",   "",           base) -> "sh."     // the repo root
/// ```
///
/// The bundled structural scanner emits each module identity repo-relative to
/// the repository base — EMPTY at the repo root. Rooting an empty identity
/// therefore renders the bare `<language>.` root (`md.`, `sh.`, …), which is
/// the repo-root module, never a checkout-named module (`md.apg`); this is the
/// ingestor half of the `md.apg` → `md.` relocation (the emitter half is the
/// scanner's `repo_relative_dir`). Every non-root structural identity roots
/// under its stream id exactly like a code module (`md.docs`, `yaml.ci`).
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
                    //
                    // A STRUCTURAL stream's EMPTY module identity is the
                    // repository root, not the scanner anomaly `rooted_scope`
                    // leaves empty for a code declaration: root it to the bare
                    // `<language>.` root module so the `Module → File` edge from
                    // the repo-root module to the repo-root files survives
                    // (`md.` ⊃ the repo-root Markdown files). A code stream's
                    // empty parent stays empty (the anomaly).
                    let parent =
                        if parent.is_empty() && crate::classify::is_structural_language(&lang) {
                            root_module_fqn(&lang, "", base)
                        } else {
                            rooted_scope(&lang, &parent, base)
                        };
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
mod tests;
