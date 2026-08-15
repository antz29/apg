//! Two-pass ingestion of unified-schema records into a [`Graph`], including the
//! canonical FQN renderer (SPEC §4).
//!
//! Pass 1 buffers node records and renders canonical FQNs (module verbatim,
//! struct `parent.name`, function `parent.name` / `parent.name(T1,T2)`, Go
//! `init` → `parent.init#<file-basename>`), building both `id → FQN` and
//! `FQN → Node` maps. Pass 2 resolves edge endpoints against those maps.
//!
//! The renderer fails loudly (panics) on any residual FQN collision rather than
//! silently overwriting.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::PathBuf;

use crate::classify::{classify_code_type, ApgConfig};
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
/// already rendered the same FQN.
fn claim(seen: &mut HashMap<String, String>, id: &str, fqn: &str) {
    if let Some(prev) = seen.get(fqn) {
        if prev != id {
            panic!("FQN collision: `{fqn}` claimed by both `{prev}` and `{id}`");
        }
    } else {
        seen.insert(fqn.to_string(), id.to_string());
    }
}

/// Renders the FQN of every function declaration (SPEC §4).
///
/// Declarations are grouped by `(parent, name)`: a singleton group renders
/// `parent.name`, an overloaded group renders `parent.name(T1,T2,...)` for every
/// member. Go `init` functions carry no signature, so each is rendered
/// `parent.init#<file-basename>` instead.
fn render_function_fqns(decls: &[FuncDecl], language: &str) -> Vec<(String, String)> {
    let mut groups: HashMap<(&str, &str), Vec<usize>> = HashMap::new();
    for (i, d) in decls.iter().enumerate() {
        groups
            .entry((d.parent.as_str(), d.name.as_str()))
            .or_default()
            .push(i);
    }

    let mut out = Vec::with_capacity(decls.len());
    for ((parent, name), idxs) in groups {
        if language == "go" && name == "init" {
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
                out.push((d.id.clone(), format!("{parent}.{name}({})", d.params.join(","))));
            }
        }
    }
    out
}

fn insert_node(graph: &mut Graph, fqn: String, node: Node) {
    if graph.nodes.contains_key(&fqn) {
        panic!("duplicate project node FQN: `{fqn}`");
    }
    graph.nodes.insert(fqn, node);
}

pub fn ingest(
    records: impl IntoIterator<Item = Record>,
    opts: &IngestOptions,
) -> (Graph, IngestReport) {
    let mut graph = Graph::default();
    let mut skipped = 0u64;
    let mut id_to_fqn: HashMap<String, String> = HashMap::new();
    let mut seen: HashMap<String, String> = HashMap::new();
    let mut funcs: Vec<FuncDecl> = Vec::new();

    // Stream the records in one pass: modules, structs, and unresolved targets
    // have deterministic FQNs and enter the graph immediately; functions are
    // buffered (overload grouping needs every declaration); edges are spooled
    // to a temp file and resolved in a second pass once ids are known. This
    // keeps memory bounded for large projects instead of buffering every
    // record (SPEC §6).
    let spool = std::env::temp_dir().join(format!(
        "apg-edge-spool-{}-{}",
        std::process::id(),
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
                    if seen.contains_key(&fqn) {
                        continue;
                    }
                    claim(&mut seen, &fqn, &fqn);
                    insert_node(
                        &mut graph,
                        fqn,
                        Node {
                            kind: NodeKind::Module,
                            location: None,
                            category: None,
                            code_type: String::new(),
                        },
                    );
                }
                Record::Struct {
                    id,
                    parent,
                    name,
                    path,
                    start,
                    end,
                } => {
                    let fqn = format!("{parent}.{name}");
                    claim(&mut seen, &id, &fqn);
                    id_to_fqn.insert(id.clone(), fqn.clone());
                    if is_blacklisted(&fqn, opts.blacklist) {
                        skipped += 1;
                        continue;
                    }
                    let code_type = classify_code_type(&path, &fqn, opts.language, opts.config);
                    insert_node(
                        &mut graph,
                        fqn,
                        Node {
                            kind: NodeKind::Struct,
                            location: Some(Location {
                                path: PathBuf::from(&path),
                                start,
                                end,
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
                } => funcs.push(FuncDecl {
                    id,
                    parent,
                    name,
                    params,
                    file,
                    path,
                    start,
                    end,
                }),
                Record::Unresolved { fqn, category } => {
                    graph.nodes.entry(fqn).or_insert_with(|| Node {
                        kind: NodeKind::UnresolvedTarget,
                        location: None,
                        category,
                        code_type: String::new(),
                    });
                }
                edge => write_edge(&mut sw, edge),
            }
        }
    }

    // Pass B: render function FQNs and insert function nodes.
    for (id, fqn) in render_function_fqns(&funcs, opts.language) {
        claim(&mut seen, &id, &fqn);
        id_to_fqn.insert(id, fqn);
    }
    for f in &funcs {
        let fqn = id_to_fqn[&f.id].clone();
        if is_blacklisted(&fqn, opts.blacklist) {
            skipped += 1;
            continue;
        }
        let code_type = classify_code_type(&f.path, &fqn, opts.language, opts.config);
        insert_node(
            &mut graph,
            fqn,
            Node {
                kind: NodeKind::Function,
                location: Some(Location {
                    path: PathBuf::from(&f.path),
                    start: f.start,
                    end: f.end,
                }),
                category: None,
                code_type,
            },
        );
    }

    // Pass C: resolve edge endpoints from the spool.
    let resolve = |s: &str| -> String { id_to_fqn.get(s).cloned().unwrap_or_else(|| s.to_string()) };
    {
        let mut er = EdgeReader {
            r: BufReader::new(std::fs::File::open(&spool).unwrap()),
        };
        while let Some(e) = er.next_edge() {
            match e {
                Record::Contains { from, to } => {
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
    graph.contains.retain(|(a, b)| {
        graph.nodes.contains_key(a)
            && graph.nodes.contains_key(b)
            && matches!(
                (graph.nodes[a].kind, graph.nodes[b].kind),
                (NodeKind::Module, NodeKind::Module)
                    | (NodeKind::Module, NodeKind::Struct)
                    | (NodeKind::Module, NodeKind::Function)
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

    (graph, IngestReport { skipped })
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
        }
    }

    fn fqns(decls: &[FuncDecl], language: &str) -> HashMap<String, String> {
        render_function_fqns(decls, language).into_iter().collect()
    }

    #[test]
    fn unique_function_keeps_simple_name() {
        let decls = [fd("n1", "pkg", "foo", &[], "/x/a.go")];
        let m = fqns(&decls, "go");
        assert_eq!(m["n1"], "pkg.foo");
    }

    #[test]
    fn overloads_get_param_suffix() {
        let decls = [
            fd("n1", "pkg.C", "foo", &["int"], "/x/a.go"),
            fd("n2", "pkg.C", "foo", &["java.lang.String"], "/x/a.go"),
        ];
        let m = fqns(&decls, "go");
        assert_eq!(m["n1"], "pkg.C.foo(int)");
        assert_eq!(m["n2"], "pkg.C.foo(java.lang.String)");
    }

    #[test]
    fn go_init_disambiguated_by_file() {
        let decls = [
            fd("n1", "pkg", "init", &[], "/x/a.go"),
            fd("n2", "pkg", "init", &[], "/x/b.go"),
        ];
        let m = fqns(&decls, "go");
        assert_eq!(m["n1"], "pkg.init#a.go");
        assert_eq!(m["n2"], "pkg.init#b.go");
    }

    #[test]
    fn zero_param_overload_gets_empty_suffix() {
        let decls = [
            fd("n1", "pkg.C", "foo", &[], "/x/a.go"),
            fd("n2", "pkg.C", "foo", &["int"], "/x/a.go"),
        ];
        let m = fqns(&decls, "go");
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
            Record::Struct {
                id: "n1".to_string(),
                parent: "pkg".to_string(),
                name: "A".to_string(),
                path: "/x/a.go".to_string(),
                start: 0,
                end: 1,
            },
            Record::Struct {
                id: "n2".to_string(),
                parent: "pkg".to_string(),
                name: "A".to_string(),
                path: "/x/b.go".to_string(),
                start: 0,
                end: 1,
            },
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
    fn end_to_end_ingest_resolves_edges() {
        let records = vec![
            Record::Module {
                fqn: "github.com/x/y".to_string(),
            },
            Record::Struct {
                id: "n1".to_string(),
                parent: "github.com/x/y".to_string(),
                name: "Store".to_string(),
                path: "/abs/store.go".to_string(),
                start: 0,
                end: 100,
            },
            Record::Function {
                id: "n2".to_string(),
                parent: "github.com/x/y".to_string(),
                name: "Compute".to_string(),
                params: vec!["[]byte".to_string(), "int".to_string()],
                file: "/abs/store.go".to_string(),
                path: "/abs/store.go".to_string(),
                start: 1,
                end: 99,
            },
            Record::Unresolved {
                fqn: "fmt.Errorf".to_string(),
                category: Some("stdlib".to_string()),
            },
            Record::Contains {
                from: "github.com/x/y".to_string(),
                to: "n1".to_string(),
            },
            Record::Contains {
                from: "n1".to_string(),
                to: "n2".to_string(),
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
        assert!(graph.nodes.contains_key("fmt.Errorf"));
        assert!(graph
            .contains
            .contains(&("github.com/x/y".to_string(), "github.com/x/y.Store".to_string())));
        assert!(graph
            .contains
            .contains(&("github.com/x/y.Store".to_string(), "github.com/x/y.Compute".to_string())));
        assert!(graph
            .unresolved_calls
            .contains(&("github.com/x/y.Compute".to_string(), "fmt.Errorf".to_string(), String::new())));
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
            Record::Struct {
                id: "n1".to_string(),
                parent: "keep.mod".to_string(),
                name: "A".to_string(),
                path: "/x/a.go".to_string(),
                start: 0,
                end: 1,
            },
            Record::Struct {
                id: "n2".to_string(),
                parent: "drop.mod".to_string(),
                name: "B".to_string(),
                path: "/x/b.go".to_string(),
                start: 0,
                end: 1,
            },
            Record::Contains {
                from: "keep.mod".to_string(),
                to: "n2".to_string(),
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
        assert!(report.skipped >= 2);
        assert!(graph.nodes.contains_key("keep.mod"));
        assert!(!graph.nodes.contains_key("drop.mod"));
        assert!(!graph.nodes.contains_key("drop.mod.B"));
        // contains edge referencing the dropped module is gone.
        assert!(graph.contains.is_empty());
    }
}
