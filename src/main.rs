mod graph;
mod lsp;

use anyhow::Result;
use graph::{EdgeKind, Graph, NodeKind};
use lsp::LspClient;
use lsp_types::{CallHierarchyItem, DocumentSymbol, SymbolKind, Uri};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;

#[derive(Clone)]
struct MethodRec {
    fqn: String,
    name: String,
    kind: SymbolKind,
    range: lsp_types::Range,
    selection_range: lsp_types::Range,
}

fn find_java_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|s| s.to_str()) == Some("java") {
                files.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
}

fn path_to_uri(path: &Path) -> Uri {
    let url = url::Url::from_file_path(path).unwrap();
    Uri::from_str(url.as_str()).unwrap()
}

fn package_from_path(project_dir: &Path, file: &Path) -> String {
    let relative = file.strip_prefix(project_dir).unwrap();
    let mut parts: Vec<&str> = Vec::new();
    for component in relative.parent().unwrap_or(Path::new("")).components() {
        use std::path::Component;
        if let Component::Normal(s) = component {
            let s = s.to_str().unwrap();
            if s == "java" || s == "main" || s == "test" {
                continue;
            }
            parts.push(s);
        }
    }
    parts.join(".")
}

fn ensure_package_hierarchy(
    graph: &mut Graph,
    package: &str,
    fqn_to_id: &mut HashMap<String, usize>,
) {
    if package.is_empty() || fqn_to_id.contains_key(package) {
        return;
    }
    let parent = package.rsplit_once('.').map(|(p, _)| p).unwrap_or("");
    if !parent.is_empty() {
        ensure_package_hierarchy(graph, parent, fqn_to_id);
    }
    let id = graph.add_node(package.to_string(), NodeKind::Package, None);
    fqn_to_id.insert(package.to_string(), id);
    if !parent.is_empty() {
        let parent_id = fqn_to_id[parent];
        graph.add_edge(parent_id, id, EdgeKind::Contains);
    }
}

fn symbol_kind(kind: SymbolKind) -> Option<NodeKind> {
    match kind {
        SymbolKind::CLASS => Some(NodeKind::Class),
        SymbolKind::INTERFACE => Some(NodeKind::Interface),
        SymbolKind::ENUM => Some(NodeKind::Enum),
        SymbolKind::STRUCT => Some(NodeKind::Record),
        SymbolKind::METHOD => Some(NodeKind::Method),
        SymbolKind::CONSTRUCTOR => Some(NodeKind::Constructor),
        SymbolKind::FIELD => Some(NodeKind::Field),
        _ => None,
    }
}

fn process_symbols(
    graph: &mut Graph,
    symbols: &[DocumentSymbol],
    parent_fqn: &str,
    file_uri_str: &str,
    fqn_to_id: &mut HashMap<String, usize>,
    lookup: &mut HashMap<(String, u32), MethodRec>,
    parent_id: usize,
) {
    for symbol in symbols {
        let fqn = format!("{}.{}", parent_fqn, symbol.name);
        dbg!(&fqn);
        if let Some(kind) = symbol_kind(symbol.kind) {
            let id = {
                let entry = fqn_to_id.entry(fqn.clone());
                *entry.or_insert_with(|| {
                    let file = Some(file_uri_str.to_string());
                    graph.add_node(fqn.clone(), kind, file)
                })
            };
            graph.add_edge(parent_id, id, EdgeKind::Contains);

            if kind == NodeKind::Method || kind == NodeKind::Constructor {
                lookup.insert(
                    (file_uri_str.to_string(), symbol.selection_range.start.line),
                    MethodRec {
                        fqn: fqn.clone(),
                        name: symbol.name.clone(),
                        kind: symbol.kind,
                        range: symbol.range,
                        selection_range: symbol.selection_range,
                    },
                );
            }

            if let Some(children) = &symbol.children {
                process_symbols(graph, children, &fqn, file_uri_str, fqn_to_id, lookup, id);
            }
        } else if let Some(children) = &symbol.children {
            process_symbols(graph, children, parent_fqn, file_uri_str, fqn_to_id, lookup, parent_id);
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let project_dir = if args.len() > 1 {
        PathBuf::from(&args[1])
    } else {
        PathBuf::from("project")
    };

    let jdtls_cmd = std::env::var("JDTLS_CMD").unwrap_or_else(|_| "jdtls".to_string());
    let project_dir = project_dir.canonicalize()?;
    eprintln!("Project: {}", project_dir.display());

    let files = find_java_files(&project_dir)?;
    eprintln!("Found {} .java files", files.len());

    let root_uri = path_to_uri(&project_dir);
    let project_root_str = root_uri.as_str().to_string();

    let mut lsp = LspClient::spawn(&jdtls_cmd).await?;
    lsp.initialize(&root_uri).await?;
    lsp.initialized().await?;
    eprintln!("JDT LS ready");

    // Open all files
    for file in &files {
        dbg!(file);
        let uri = path_to_uri(file);
        let content = tokio::fs::read_to_string(file).await?;
        lsp.did_open(&uri, &content).await?;
    }
    eprintln!("Opened {} files", files.len());

    // Get document symbols for each file
    let mut file_symbols: Vec<(PathBuf, Vec<DocumentSymbol>)> = Vec::new();
    for file in &files {
        let uri = path_to_uri(file);
        dbg!(file);
        match lsp.document_symbols(&uri).await {
            Ok(symbols) if !symbols.is_empty() => file_symbols.push((file.clone(), symbols)),
            Ok(_) => eprintln!("  warn: {}: no symbols returned", file.display()),
            Err(e) => eprintln!("  warn: {}: {e}", file.display()),
        }
    }
    eprintln!("Got symbols from {} files", file_symbols.len());

    // Build symbol hierarchy
    let mut graph = Graph::new();
    let mut fqn_to_id: HashMap<String, usize> = HashMap::new();
    let mut lookup: HashMap<(String, u32), MethodRec> = HashMap::new();

    for (file, symbols) in &file_symbols {
        let package = package_from_path(&project_dir, file);
        ensure_package_hierarchy(&mut graph, &package, &mut fqn_to_id);
        let file_uri = path_to_uri(file);
        let file_uri_str = file_uri.as_str().to_string();
        let package_id = fqn_to_id[&package];
        process_symbols(
            &mut graph,
            symbols,
            &package,
            &file_uri_str,
            &mut fqn_to_id,
            &mut lookup,
            package_id,
        );
    }
    eprintln!(
        "Symbol hierarchy: {} nodes, {} methods/ctors",
        graph.nodes.len(),
        lookup.len()
    );

    // Call hierarchy — construct CallHierarchyItem directly from symbol data
    let methods: Vec<(Uri, MethodRec)> = lookup
        .iter()
        .map(|((uri_str, _), rec)| (Uri::from_str(uri_str).unwrap(), rec.clone()))
        .collect();
    let total = methods.len();

    for (i, (uri, rec)) in methods.iter().enumerate() {
        if (i + 1) % 100 == 0 || i == 0 {
            eprintln!("Calls: {}/{}", i + 1, total);
        }

        let item = CallHierarchyItem {
            name: rec.name.clone(),
            kind: rec.kind,
            tags: None,
            detail: None,
            uri: uri.clone(),
            range: rec.range,
            selection_range: rec.selection_range,
            data: None,
        };

        let calls = match lsp.outgoing_calls(item).await {
            Ok(calls) => calls,
            Err(_) => continue,
        };

        let source_id = match fqn_to_id.get(&rec.fqn) {
            Some(&id) => id,
            None => continue,
        };

        for call in calls {
            let tu = call.to.uri.as_str();
            if !tu.starts_with(&project_root_str) {
                continue;
            }
            let tl = call.to.selection_range.start.line;
            if let Some(target_rec) = lookup.get(&(call.to.uri.as_str().to_string(), tl)) {
                if let Some(&target_id) = fqn_to_id.get(&target_rec.fqn) {
                    eprintln!("{} -> {}", &rec.fqn, &target_rec.fqn);
                    graph.add_edge(source_id, target_id, EdgeKind::Calls);
                }
            }
        }
    }
    eprintln!("Call graph built: {} total edges",
        graph.nodes.iter().map(|n| n.edges.len()).sum::<usize>()
    );

    let json = serde_json::to_string_pretty(&graph)?;
    println!("{json}");

    lsp.shutdown().await?;
    Ok(())
}
