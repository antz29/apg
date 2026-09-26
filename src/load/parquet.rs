//! The low-level PARQUET sink for the load files: the column model, the
//! generic writer, and the graph location/line accessors shared by the load
//! tables and the `graph.jsonl` export.

use std::fs::File;
use std::path::Path;
use std::sync::Arc;

use parquet::basic::{Compression, ConvertedType, Repetition, Type as PhysicalType};
use parquet::data_type::{ByteArray, ByteArrayType, Int64Type};
use parquet::file::properties::WriterProperties;
use parquet::file::writer::SerializedFileWriter;
use parquet::schema::types::Type;

use crate::graph::Graph;

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

pub(crate) fn loc(graph: &Graph, fqn: &str) -> (String, i64, i64) {
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

pub(crate) fn lines(graph: &Graph, fqn: &str) -> (i64, i64) {
    graph
        .nodes
        .get(fqn)
        .and_then(|n| n.location.as_ref())
        .map(|l| (l.start_line as i64, l.end_line as i64))
        .unwrap_or_default()
}
