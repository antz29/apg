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

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::PathBuf;

use crate::classify::{ApgConfig, classify_code_type};
use crate::graph::{Graph, Location, Node, NodeKind};
use crate::schema::Record;

pub struct IngestOptions<'a> {
    pub blacklist: &'a [String],
    pub language: &'a str,
    pub config: Option<&'a ApgConfig>,
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
        Some(_) => panic!("duplicate project node FQN: `{fqn}`"),
    }
    graph.nodes.insert(fqn, node);
}

pub fn ingest(
    records: impl IntoIterator<Item = Record>,
    opts: &IngestOptions,
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
                    });
                }
                Record::LangSwitch { language } => {
                    lang = language;
                }
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
                category: None,
                code_type,
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
                _ => unreachable!("non-edge record reached the edge pass"),
            }
        }
    }
    let _ = std::fs::remove_file(&spool);

    // Drop edges whose endpoints do not exist (dangling ids, blacklisted nodes).
    // Containment is a strict tree: Module→Module, Module→File, File→Struct,
    // File→Function, Struct→Struct, Struct→Function (SPEC §7).
    graph.contains.retain(|(a, b)| {
        graph.nodes.contains_key(a)
            && graph.nodes.contains_key(b)
            && matches!(
                (graph.nodes[a].kind, graph.nodes[b].kind),
                (NodeKind::Module, NodeKind::Module)
                    | (NodeKind::Module, NodeKind::File)
                    | (NodeKind::File, NodeKind::Struct)
                    | (NodeKind::File, NodeKind::Function)
                    | (NodeKind::Struct, NodeKind::Struct)
                    | (NodeKind::Struct, NodeKind::Function)
            )
    });
    graph.calls.retain(|(a, b)| {
        graph.nodes.contains_key(a)
            && graph.nodes.contains_key(b)
            && graph.nodes[a].kind == NodeKind::Function
            && graph.nodes[b].kind == NodeKind::Function
    });
    graph.uses.retain(|(a, b)| {
        graph.nodes.contains_key(a)
            && graph.nodes.contains_key(b)
            && graph.nodes[b].kind == NodeKind::Struct
            && matches!(graph.nodes[a].kind, NodeKind::Function | NodeKind::Struct)
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

    (
        graph,
        IngestReport {
            skipped,
            shadowed_modules,
            shadowed_functions,
        },
    )
}

/// Binary spool format for edge records: one u8 tag (0 contains, 1 calls,
/// 2 uses, 3 unresolved_call, 4 unresolved_use) followed by three length-
/// prefixed UTF-8 strings (from, to, target_type; the last empty for most).
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
}
