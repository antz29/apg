//! Bulk-load of a [`Graph`] into `db.lbug` via `COPY FROM` PARQUET load files
//! (SPEC §6 step 4), plus the `graph.jsonl` export (step 5).
//!
//! Load files are written with the low-level `parquet` writer so that string
//! columns carry the legacy `ConvertedType::UTF8` annotation. lbug 0.19.1's
//! PARQUET reader derives logical types from `converted_type` only, so the
//! arrow-rs default (`LogicalType::String`) would be misread as `BLOB`.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::Arc;

use lbug::Connection;
use parquet::basic::{Compression, ConvertedType, Repetition, Type as PhysicalType};
use parquet::data_type::{ByteArray, ByteArrayType, Int64Type};
use parquet::file::properties::WriterProperties;
use parquet::file::writer::SerializedFileWriter;
use parquet::schema::types::Type;
use serde::Serialize;

use crate::graph::{Graph, NodeKind};

pub enum Col {
    Str(Vec<String>),
    I64(Vec<i64>),
}

fn rows_len(cols: &[(&str, Col)]) -> usize {
    cols.first()
        .map(|(_, c)| match c {
            Col::Str(v) => v.len(),
            Col::I64(v) => v.len(),
        })
        .unwrap_or(0)
}

/// Writes a single PARQUET file from named columns. Every column must have the
/// same length. String columns are written as `BYTE_ARRAY` with
/// `ConvertedType::UTF8`; integer columns as `INT64`.
pub fn write_parquet(path: &Path, cols: &[(&str, Col)]) -> anyhow::Result<()> {
    let n = rows_len(cols);
    debug_assert!(cols.iter().all(|(_, c)| match c {
        Col::Str(v) => v.len() == n,
        Col::I64(v) => v.len() == n,
    }));

    let mut fields: Vec<Arc<Type>> = Vec::with_capacity(cols.len());
    for (name, c) in cols {
        let builder = Type::primitive_type_builder(name, physical_of(c));
        let builder = match c {
            Col::Str(_) => builder.with_converted_type(ConvertedType::UTF8),
            Col::I64(_) => builder,
        };
        fields.push(Arc::new(
            builder.with_repetition(Repetition::REQUIRED).build()?,
        ));
    }
    let schema = Arc::new(
        Type::group_type_builder("schema")
            .with_fields(fields)
            .build()?,
    );

    let file = File::create(path)?;
    let props = Arc::new(
        WriterProperties::builder()
            .set_compression(Compression::SNAPPY)
            .build(),
    );
    let mut writer = SerializedFileWriter::new(file, schema, props)?;
    {
        let mut row_group = writer.next_row_group()?;
        for (_, c) in cols {
            let col_writer = row_group.next_column()?.expect("column expected");
            match c {
                Col::Str(vals) => {
                    let values: Vec<ByteArray> =
                        vals.iter().map(|s| ByteArray::from(s.as_str())).collect();
                    let mut typed = col_writer;
                    typed
                        .typed::<ByteArrayType>()
                        .write_batch(&values, None, None)?;
                    typed.close()?;
                }
                Col::I64(vals) => {
                    let mut typed = col_writer;
                    typed.typed::<Int64Type>().write_batch(vals, None, None)?;
                    typed.close()?;
                }
            }
        }
        row_group.close()?;
    }
    writer.close()?;
    Ok(())
}

fn physical_of(c: &Col) -> PhysicalType {
    match c {
        Col::Str(_) => PhysicalType::BYTE_ARRAY,
        Col::I64(_) => PhysicalType::INT64,
    }
}

fn loc(graph: &Graph, fqn: &str) -> (String, i64, i64) {
    graph
        .nodes
        .get(fqn)
        .and_then(|n| n.location.as_ref())
        .map(|l| {
            (
                l.path.to_string_lossy().into_owned(),
                l.start as i64,
                l.end as i64,
            )
        })
        .unwrap_or_default()
}

fn lines(graph: &Graph, fqn: &str) -> (i64, i64) {
    graph
        .nodes
        .get(fqn)
        .and_then(|n| n.location.as_ref())
        .map(|l| (l.start_line as i64, l.end_line as i64))
        .unwrap_or_default()
}

/// Writes one PARQUET file per node table and one per rel-table `(from, to)`
/// pair into `dir`. Columns match the LadybugDB table schema exactly.
pub fn build_load_files(graph: &Graph, dir: &Path) -> anyhow::Result<()> {
    // --- Node tables ---
    let mut module_fqn = Vec::new();
    let mut struct_fqn = Vec::new();
    let mut struct_path = Vec::new();
    let mut struct_start = Vec::new();
    let mut struct_end = Vec::new();
    let mut struct_start_line = Vec::new();
    let mut struct_end_line = Vec::new();
    let mut struct_ct = Vec::new();
    let mut fn_fqn = Vec::new();
    let mut fn_path = Vec::new();
    let mut fn_start = Vec::new();
    let mut fn_end = Vec::new();
    let mut fn_start_line = Vec::new();
    let mut fn_end_line = Vec::new();
    let mut fn_ct = Vec::new();
    let mut file_fqn = Vec::new();
    let mut file_start_line = Vec::new();
    let mut file_end_line = Vec::new();
    let mut file_ct = Vec::new();
    let mut unres_fqn = Vec::new();
    let mut unres_cat = Vec::new();

    for (fqn, node) in &graph.nodes {
        match node.kind {
            NodeKind::Module => module_fqn.push(fqn.clone()),
            NodeKind::Struct => {
                struct_fqn.push(fqn.clone());
                let (p, s, e) = loc(graph, fqn);
                let (sl, el) = lines(graph, fqn);
                struct_path.push(p);
                struct_start.push(s);
                struct_end.push(e);
                struct_start_line.push(sl);
                struct_end_line.push(el);
                struct_ct.push(node.code_type.clone());
            }
            NodeKind::Function => {
                fn_fqn.push(fqn.clone());
                let (p, s, e) = loc(graph, fqn);
                let (sl, el) = lines(graph, fqn);
                fn_path.push(p);
                fn_start.push(s);
                fn_end.push(e);
                fn_start_line.push(sl);
                fn_end_line.push(el);
                fn_ct.push(node.code_type.clone());
            }
            NodeKind::File => {
                file_fqn.push(fqn.clone());
                let (sl, el) = lines(graph, fqn);
                file_start_line.push(sl);
                file_end_line.push(el);
                file_ct.push(node.code_type.clone());
            }
            NodeKind::UnresolvedTarget => {
                unres_fqn.push(fqn.clone());
                unres_cat.push(node.category.clone().unwrap_or_default());
            }
        }
    }

    write_parquet(
        &dir.join("module.parquet"),
        &[("fqn", Col::Str(module_fqn))],
    )?;
    write_parquet(
        &dir.join("struct.parquet"),
        &[
            ("fqn", Col::Str(struct_fqn)),
            ("path", Col::Str(struct_path)),
            ("start", Col::I64(struct_start)),
            ("end", Col::I64(struct_end)),
            ("start_line", Col::I64(struct_start_line)),
            ("end_line", Col::I64(struct_end_line)),
            ("code_type", Col::Str(struct_ct)),
        ],
    )?;
    write_parquet(
        &dir.join("function.parquet"),
        &[
            ("fqn", Col::Str(fn_fqn)),
            ("path", Col::Str(fn_path)),
            ("start", Col::I64(fn_start)),
            ("end", Col::I64(fn_end)),
            ("start_line", Col::I64(fn_start_line)),
            ("end_line", Col::I64(fn_end_line)),
            ("code_type", Col::Str(fn_ct)),
        ],
    )?;
    write_parquet(
        &dir.join("file.parquet"),
        &[
            ("fqn", Col::Str(file_fqn)),
            ("start_line", Col::I64(file_start_line)),
            ("end_line", Col::I64(file_end_line)),
            ("code_type", Col::Str(file_ct)),
        ],
    )?;
    write_parquet(
        &dir.join("unresolved.parquet"),
        &[
            ("fqn", Col::Str(unres_fqn)),
            ("category", Col::Str(unres_cat)),
        ],
    )?;

    // --- Rel tables ---
    let mut c_mm = (Vec::new(), Vec::new());
    let mut c_mfile = (Vec::new(), Vec::new());
    let mut c_fs = (Vec::new(), Vec::new());
    let mut c_ff = (Vec::new(), Vec::new());
    let mut c_ss = (Vec::new(), Vec::new());
    let mut c_sf = (Vec::new(), Vec::new());
    for (a, b) in &graph.contains {
        let dst = match (graph.nodes[a].kind, graph.nodes[b].kind) {
            (NodeKind::Module, NodeKind::Module) => &mut c_mm,
            (NodeKind::Module, NodeKind::File) => &mut c_mfile,
            (NodeKind::File, NodeKind::Struct) => &mut c_fs,
            (NodeKind::File, NodeKind::Function) => &mut c_ff,
            (NodeKind::Struct, NodeKind::Struct) => &mut c_ss,
            (NodeKind::Struct, NodeKind::Function) => &mut c_sf,
            _ => unreachable!("unvalidated contains edge"),
        };
        dst.0.push(a.clone());
        dst.1.push(b.clone());
    }

    let mut calls = (Vec::new(), Vec::new());
    for (a, b) in &graph.calls {
        calls.0.push(a.clone());
        calls.1.push(b.clone());
    }

    let mut u_fn = (Vec::new(), Vec::new());
    let mut u_st = (Vec::new(), Vec::new());
    for (a, b) in &graph.uses {
        let dst = match graph.nodes[a].kind {
            NodeKind::Function => &mut u_fn,
            NodeKind::Struct => &mut u_st,
            _ => unreachable!("unvalidated uses edge"),
        };
        dst.0.push(a.clone());
        dst.1.push(b.clone());
    }

    let mut uc_from = Vec::new();
    let mut uc_to = Vec::new();
    let mut uc_tt = Vec::new();
    for (a, b, t) in &graph.unresolved_calls {
        uc_from.push(a.clone());
        uc_to.push(b.clone());
        uc_tt.push(t.clone());
    }

    let mut uu_fn = (Vec::new(), Vec::new());
    let mut uu_st = (Vec::new(), Vec::new());
    for (a, b) in &graph.unresolved_uses {
        let dst = match graph.nodes[a].kind {
            NodeKind::Function => &mut uu_fn,
            NodeKind::Struct => &mut uu_st,
            _ => unreachable!("unvalidated unresolved_use edge"),
        };
        dst.0.push(a.clone());
        dst.1.push(b.clone());
    }

    let rel = |name: &str, from: Vec<String>, to: Vec<String>| -> anyhow::Result<()> {
        write_parquet(
            &dir.join(name),
            &[("from", Col::Str(from)), ("to", Col::Str(to))],
        )
    };
    rel("contains_mod_mod.parquet", c_mm.0, c_mm.1)?;
    rel("contains_mod_file.parquet", c_mfile.0, c_mfile.1)?;
    rel("contains_file_struct.parquet", c_fs.0, c_fs.1)?;
    rel("contains_file_fn.parquet", c_ff.0, c_ff.1)?;
    rel("contains_struct_struct.parquet", c_ss.0, c_ss.1)?;
    rel("contains_struct_fn.parquet", c_sf.0, c_sf.1)?;
    rel("calls.parquet", calls.0, calls.1)?;
    rel("uses_fn.parquet", u_fn.0, u_fn.1)?;
    rel("uses_struct.parquet", u_st.0, u_st.1)?;
    write_parquet(
        &dir.join("unresolved_call.parquet"),
        &[
            ("from", Col::Str(uc_from)),
            ("to", Col::Str(uc_to)),
            ("target_type", Col::Str(uc_tt)),
        ],
    )?;
    rel("unresolved_use_fn.parquet", uu_fn.0, uu_fn.1)?;
    rel("unresolved_use_struct.parquet", uu_st.0, uu_st.1)?;

    Ok(())
}

/// Creates the LadybugDB schema (SPEC §7): five node tables and five rel tables.
pub fn create_schema(conn: &Connection) -> anyhow::Result<()> {
    conn.query("CREATE NODE TABLE Module(fqn STRING PRIMARY KEY)")?;
    conn.query(
        "CREATE NODE TABLE Struct(fqn STRING PRIMARY KEY, path STRING, start INT64, `end` INT64, start_line INT64, end_line INT64, code_type STRING)",
    )?;
    conn.query(
        "CREATE NODE TABLE Function(fqn STRING PRIMARY KEY, path STRING, start INT64, `end` INT64, start_line INT64, end_line INT64, code_type STRING)",
    )?;
    conn.query(
        "CREATE NODE TABLE File(fqn STRING PRIMARY KEY, start_line INT64, end_line INT64, code_type STRING)",
    )?;
    conn.query("CREATE NODE TABLE UnresolvedTarget(fqn STRING PRIMARY KEY, category STRING)")?;
    conn.query(
        "CREATE REL TABLE Contains(FROM Module TO Module, FROM Module TO File, FROM File TO Struct, FROM File TO Function, FROM Struct TO Struct, FROM Struct TO Function)",
    )?;
    conn.query("CREATE REL TABLE Calls(FROM Function TO Function)")?;
    conn.query("CREATE REL TABLE Uses(FROM Function TO Struct, FROM Struct TO Struct)")?;
    conn.query(
        "CREATE REL TABLE UnresolvedCall(FROM Function TO UnresolvedTarget, target_type STRING)",
    )?;
    conn.query(
        "CREATE REL TABLE UnresolvedUse(FROM Function TO UnresolvedTarget, FROM Struct TO UnresolvedTarget)",
    )?;
    Ok(())
}

/// Loads all PARQUET files in `dir` via `COPY FROM`, per `(from, to)` pair for
/// multi-pair rel tables.
pub fn copy_from(conn: &Connection, dir: &Path) -> anyhow::Result<()> {
    let p = |name: &str| dir.join(name).to_string_lossy().into_owned();
    let stmts = [
        format!(r#"COPY Module FROM "{}""#, p("module.parquet")),
        format!(r#"COPY Struct FROM "{}""#, p("struct.parquet")),
        format!(r#"COPY Function FROM "{}""#, p("function.parquet")),
        format!(r#"COPY File FROM "{}""#, p("file.parquet")),
        format!(
            r#"COPY UnresolvedTarget FROM "{}""#,
            p("unresolved.parquet")
        ),
        format!(
            r#"COPY Contains FROM "{}" (from="Module", to="Module")"#,
            p("contains_mod_mod.parquet")
        ),
        format!(
            r#"COPY Contains FROM "{}" (from="Module", to="File")"#,
            p("contains_mod_file.parquet")
        ),
        format!(
            r#"COPY Contains FROM "{}" (from="File", to="Struct")"#,
            p("contains_file_struct.parquet")
        ),
        format!(
            r#"COPY Contains FROM "{}" (from="File", to="Function")"#,
            p("contains_file_fn.parquet")
        ),
        format!(
            r#"COPY Contains FROM "{}" (from="Struct", to="Struct")"#,
            p("contains_struct_struct.parquet")
        ),
        format!(
            r#"COPY Contains FROM "{}" (from="Struct", to="Function")"#,
            p("contains_struct_fn.parquet")
        ),
        format!(r#"COPY Calls FROM "{}""#, p("calls.parquet")),
        format!(
            r#"COPY Uses FROM "{}" (from="Function", to="Struct")"#,
            p("uses_fn.parquet")
        ),
        format!(
            r#"COPY Uses FROM "{}" (from="Struct", to="Struct")"#,
            p("uses_struct.parquet")
        ),
        format!(
            r#"COPY UnresolvedCall FROM "{}""#,
            p("unresolved_call.parquet")
        ),
        format!(
            r#"COPY UnresolvedUse FROM "{}" (from="Function", to="UnresolvedTarget")"#,
            p("unresolved_use_fn.parquet")
        ),
        format!(
            r#"COPY UnresolvedUse FROM "{}" (from="Struct", to="UnresolvedTarget")"#,
            p("unresolved_use_struct.parquet")
        ),
    ];
    for s in stmts {
        conn.query(&s)?;
    }
    Ok(())
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Export {
    Module {
        fqn: String,
    },
    Struct {
        fqn: String,
        path: String,
        start: u32,
        end: u32,
        start_line: u32,
        end_line: u32,
        code_type: String,
    },
    Function {
        fqn: String,
        path: String,
        start: u32,
        end: u32,
        start_line: u32,
        end_line: u32,
        code_type: String,
    },
    File {
        fqn: String,
        start_line: u32,
        end_line: u32,
        code_type: String,
    },
    Unresolved {
        fqn: String,
        category: String,
    },
    Contains {
        from: String,
        to: String,
    },
    Calls {
        from: String,
        to: String,
    },
    Uses {
        from: String,
        to: String,
    },
    UnresolvedCall {
        from: String,
        to: String,
        target_type: String,
    },
    UnresolvedUse {
        from: String,
        to: String,
    },
}

/// Writes `graph.jsonl`: the final graph re-serialized with canonical FQNs
/// (nodes) and resolved endpoints (edges), without opaque ids.
pub fn write_graph_jsonl(graph: &Graph, path: &Path) -> anyhow::Result<()> {
    let file = File::create(path)?;
    let mut w = BufWriter::new(file);

    let write_line = |w: &mut BufWriter<File>, v: &Export| -> anyhow::Result<()> {
        w.write_all(serde_json::to_string(v)?.as_bytes())?;
        w.write_all(b"\n")?;
        Ok(())
    };

    for (fqn, node) in &graph.nodes {
        let rec = match node.kind {
            NodeKind::Module => Export::Module { fqn: fqn.clone() },
            NodeKind::Struct => {
                let (path, start, end) = loc(graph, fqn);
                let (start_line, end_line) = lines(graph, fqn);
                Export::Struct {
                    fqn: fqn.clone(),
                    path,
                    start: start as u32,
                    end: end as u32,
                    start_line: start_line as u32,
                    end_line: end_line as u32,
                    code_type: node.code_type.clone(),
                }
            }
            NodeKind::Function => {
                let (path, start, end) = loc(graph, fqn);
                let (start_line, end_line) = lines(graph, fqn);
                Export::Function {
                    fqn: fqn.clone(),
                    path,
                    start: start as u32,
                    end: end as u32,
                    start_line: start_line as u32,
                    end_line: end_line as u32,
                    code_type: node.code_type.clone(),
                }
            }
            NodeKind::File => {
                let (start_line, end_line) = lines(graph, fqn);
                Export::File {
                    fqn: fqn.clone(),
                    start_line: start_line as u32,
                    end_line: end_line as u32,
                    code_type: node.code_type.clone(),
                }
            }
            NodeKind::UnresolvedTarget => Export::Unresolved {
                fqn: fqn.clone(),
                category: node.category.clone().unwrap_or_default(),
            },
        };
        write_line(&mut w, &rec)?;
    }
    for (a, b) in &graph.contains {
        write_line(
            &mut w,
            &Export::Contains {
                from: a.clone(),
                to: b.clone(),
            },
        )?;
    }
    for (a, b) in &graph.calls {
        write_line(
            &mut w,
            &Export::Calls {
                from: a.clone(),
                to: b.clone(),
            },
        )?;
    }
    for (a, b) in &graph.uses {
        write_line(
            &mut w,
            &Export::Uses {
                from: a.clone(),
                to: b.clone(),
            },
        )?;
    }
    for (a, b, t) in &graph.unresolved_calls {
        write_line(
            &mut w,
            &Export::UnresolvedCall {
                from: a.clone(),
                to: b.clone(),
                target_type: t.clone(),
            },
        )?;
    }
    for (a, b) in &graph.unresolved_uses {
        write_line(
            &mut w,
            &Export::UnresolvedUse {
                from: a.clone(),
                to: b.clone(),
            },
        )?;
    }
    w.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{Graph, Location, Node};
    use lbug::{Database, SystemConfig};

    fn fixture_graph() -> Graph {
        let mut g = Graph::default();
        let node = |kind: NodeKind, loc: Option<Location>, cat: Option<&str>| Node {
            kind,
            location: loc,
            category: cat.map(str::to_string),
            code_type: "src".to_string(),
        };
        g.nodes
            .insert("mod".to_string(), node(NodeKind::Module, None, None));
        g.nodes.insert(
            "/x/a.go".to_string(),
            node(
                NodeKind::File,
                Some(Location {
                    path: "/x/a.go".into(),
                    start: 0,
                    end: 0,
                    start_line: 1,
                    end_line: 80,
                }),
                None,
            ),
        );
        g.nodes.insert(
            "mod.A".to_string(),
            node(
                NodeKind::Struct,
                Some(Location {
                    path: "/x/a.go".into(),
                    start: 0,
                    end: 50,
                    start_line: 1,
                    end_line: 50,
                }),
                None,
            ),
        );
        g.nodes.insert(
            "mod.A.f".to_string(),
            node(
                NodeKind::Function,
                Some(Location {
                    path: "/x/a.go".into(),
                    start: 1,
                    end: 49,
                    start_line: 2,
                    end_line: 49,
                }),
                None,
            ),
        );
        g.nodes.insert(
            "ext.Foo".to_string(),
            node(NodeKind::UnresolvedTarget, None, Some("external")),
        );
        g.contains
            .insert(("mod".to_string(), "/x/a.go".to_string()));
        g.contains
            .insert(("/x/a.go".to_string(), "mod.A".to_string()));
        g.contains
            .insert(("/x/a.go".to_string(), "mod.A.f".to_string()));
        g.contains
            .insert(("mod.A".to_string(), "mod.A.f".to_string()));
        g.calls
            .insert(("mod.A.f".to_string(), "mod.A.f".to_string()));
        g.uses.insert(("mod.A.f".to_string(), "mod.A".to_string()));
        g.unresolved_calls
            .insert(("mod.A.f".to_string(), "ext.Foo".to_string(), String::new()));
        g.unresolved_uses
            .insert(("mod.A.f".to_string(), "ext.Foo".to_string()));
        g
    }

    #[test]
    fn parquet_copy_from_roundtrip() {
        let dir = std::env::temp_dir().join(format!("apg-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let graph = fixture_graph();
        build_load_files(&graph, &dir).unwrap();

        let db = Database::in_memory(SystemConfig::default()).unwrap();
        let conn = Connection::new(&db).unwrap();
        create_schema(&conn).unwrap();
        copy_from(&conn, &dir).unwrap();

        // Node tables loaded with correct columns.
        let out = conn
            .query("MATCH (s:Struct) RETURN s.fqn, s.code_type")
            .unwrap()
            .to_string();
        assert!(out.contains("mod.A"), "struct rows: {out}");
        assert!(out.contains("src"), "struct code_type: {out}");

        // `start`/`end` INT64 columns (including the reserved `end` name) and the
        // line columns load.
        let out = conn
            .query("MATCH (s:Struct) WHERE s.fqn = 'mod.A' RETURN s.start, s.`end`, s.start_line, s.end_line")
            .unwrap()
            .to_string();
        assert!(out.contains("0|50|1|50"), "struct span: {out}");

        let out = conn
            .query("MATCH (t:UnresolvedTarget) RETURN t.fqn, t.category")
            .unwrap()
            .to_string();
        assert!(
            out.contains("ext.Foo") && out.contains("external"),
            "unresolved rows: {out}"
        );

        // File node table with line columns (fqn == absolute path).
        let out = conn
            .query("MATCH (f:File) RETURN f.fqn, f.start_line, f.end_line, f.code_type")
            .unwrap()
            .to_string();
        assert!(
            out.contains("/x/a.go") && out.contains("80") && out.contains("src"),
            "file rows: {out}"
        );

        // Multi-pair rel table: Module -> File.
        let out = conn
            .query("MATCH (a:Module)-[:Contains]->(b:File) RETURN b.fqn")
            .unwrap()
            .to_string();
        assert!(out.contains("/x/a.go"), "contains Mod->File: {out}");

        // Multi-pair rel table: File -> Struct and File -> Function.
        let out = conn
            .query("MATCH (a:File)-[:Contains]->(b:Struct) RETURN b.fqn")
            .unwrap()
            .to_string();
        assert!(out.contains("mod.A"), "contains File->Struct: {out}");

        let out = conn
            .query("MATCH (a:File)-[:Contains]->(b:Function) RETURN b.fqn")
            .unwrap()
            .to_string();
        assert!(out.contains("mod.A.f"), "contains File->Function: {out}");

        // Multi-pair rel table: Struct -> Function.
        let out = conn
            .query("MATCH (a:Struct)-[:Contains]->(b:Function) RETURN b.fqn")
            .unwrap()
            .to_string();
        assert!(out.contains("mod.A.f"), "contains Struct->Function: {out}");

        // Uses with property-free rel table.
        let out = conn
            .query("MATCH (f:Function)-[:Uses]->(s:Struct) RETURN s.fqn")
            .unwrap()
            .to_string();
        assert!(out.contains("mod.A"), "uses: {out}");

        // UnresolvedCall with target_type property.
        let out = conn
            .query("MATCH (f:Function)-[r:UnresolvedCall]->(t:UnresolvedTarget) RETURN t.fqn")
            .unwrap()
            .to_string();
        assert!(out.contains("ext.Foo"), "unresolved_call: {out}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn graph_jsonl_is_valid_and_self_contained() {
        let dir = std::env::temp_dir().join(format!("apg-test-jsonl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let graph = fixture_graph();
        let out_path = dir.join("graph.jsonl");
        write_graph_jsonl(&graph, &out_path).unwrap();

        let text = std::fs::read_to_string(&out_path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines.len(),
            graph.nodes.len()
                + graph.contains.len()
                + graph.calls.len()
                + graph.uses.len()
                + graph.unresolved_calls.len()
                + graph.unresolved_uses.len()
        );
        // Every line is valid JSON with a `type` discriminator.
        for line in &lines {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            assert!(v.get("type").is_some(), "missing type: {line}");
        }

        let _ = std::fs::remove_dir_all(&dir);
    }
}
