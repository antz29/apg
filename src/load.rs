//! Bulk-load of a [`Graph`] into `db.lbug` via `COPY FROM` PARQUET load files
//! (SPEC §6 step 4), plus the `graph.jsonl` export (step 5).
//!
//! Load files are written with the low-level `parquet` writer so that string
//! columns carry the legacy `ConvertedType::UTF8` annotation. lbug 0.19.1's
//! PARQUET reader derives logical types from `converted_type` only, so the
//! arrow-rs default (`LogicalType::String`) would be misread as `BLOB`.

use std::collections::HashMap;
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
    let mut spec_fqn = Vec::new();
    let mut spec_title = Vec::new();
    let mut spec_goal = Vec::new();
    let mut req_fqn = Vec::new();
    let mut req_id = Vec::new();
    let mut req_title = Vec::new();
    let mut req_body = Vec::new();
    let mut req_feature = Vec::new();
    let mut phase_fqn = Vec::new();
    let mut phase_number = Vec::new();
    let mut phase_title = Vec::new();
    let mut decision_fqn = Vec::new();
    let mut decision_id = Vec::new();
    let mut decision_summary = Vec::new();
    let mut future_fqn = Vec::new();
    let mut future_kind = Vec::new();
    let mut future_target = Vec::new();
    let mut nongoal_fqn = Vec::new();
    let mut nongoal_body = Vec::new();
    let mut ac_fqn = Vec::new();
    let mut ac_body = Vec::new();
    let mut vi_fqn = Vec::new();
    let mut vi_body = Vec::new();
    let mut note_fqn = Vec::new();
    let mut note_body = Vec::new();
    let mut note_kind = Vec::new();
    let mut feedback_fqn = Vec::new();
    let mut feedback_body = Vec::new();
    let mut feedback_status = Vec::new();
    let mut feedback_disposition = Vec::new();
    let mut plan_fqn = Vec::new();
    let mut plan_title = Vec::new();
    let mut plan_strategy = Vec::new();
    let mut planphase_fqn = Vec::new();
    let mut planphase_number = Vec::new();
    let mut planphase_title = Vec::new();
    let mut planphase_deliverable = Vec::new();
    let mut task_fqn = Vec::new();
    let mut task_title = Vec::new();
    let mut task_kind = Vec::new();
    let mut task_tier = Vec::new();
    let mut task_status = Vec::new();

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
            NodeKind::Spec => {
                spec_fqn.push(fqn.clone());
                spec_title.push(node.title.clone().unwrap_or_default());
                spec_goal.push(node.goal.clone().unwrap_or_default());
            }
            NodeKind::Requirement => {
                req_fqn.push(fqn.clone());
                req_id.push(node.id.clone().unwrap_or_default());
                req_title.push(node.title.clone().unwrap_or_default());
                req_body.push(node.body.clone().unwrap_or_default());
                req_feature.push(node.feature.clone().unwrap_or_default());
            }
            NodeKind::Phase => {
                phase_fqn.push(fqn.clone());
                phase_number.push(node.number.map(|n| n as i64).unwrap_or_default());
                phase_title.push(node.title.clone().unwrap_or_default());
            }
            NodeKind::Decision => {
                decision_fqn.push(fqn.clone());
                decision_id.push(node.id.clone().unwrap_or_default());
                decision_summary.push(node.summary.clone().unwrap_or_default());
            }
            NodeKind::Future => {
                future_fqn.push(fqn.clone());
                future_kind.push(node.sub_kind.clone().unwrap_or_default());
                future_target.push(node.target.clone().unwrap_or_default());
            }
            NodeKind::NonGoal => {
                nongoal_fqn.push(fqn.clone());
                nongoal_body.push(node.body.clone().unwrap_or_default());
            }
            NodeKind::AcceptanceCriterion => {
                ac_fqn.push(fqn.clone());
                ac_body.push(node.body.clone().unwrap_or_default());
            }
            NodeKind::VerificationItem => {
                vi_fqn.push(fqn.clone());
                vi_body.push(node.body.clone().unwrap_or_default());
            }
            NodeKind::Note => {
                note_fqn.push(fqn.clone());
                note_body.push(node.body.clone().unwrap_or_default());
                note_kind.push(node.sub_kind.clone().unwrap_or_default());
            }
            NodeKind::Feedback => {
                feedback_fqn.push(fqn.clone());
                feedback_body.push(node.body.clone().unwrap_or_default());
                feedback_status.push(node.status.clone().unwrap_or_default());
                feedback_disposition.push(node.disposition.clone().unwrap_or_default());
            }
            NodeKind::Plan => {
                plan_fqn.push(fqn.clone());
                plan_title.push(node.title.clone().unwrap_or_default());
                plan_strategy.push(node.strategy.clone().unwrap_or_default());
            }
            NodeKind::PlanPhase => {
                planphase_fqn.push(fqn.clone());
                planphase_number.push(node.number.map(|n| n as i64).unwrap_or_default());
                planphase_title.push(node.title.clone().unwrap_or_default());
                planphase_deliverable.push(node.deliverable.clone().unwrap_or_default());
            }
            NodeKind::Task => {
                task_fqn.push(fqn.clone());
                task_title.push(node.title.clone().unwrap_or_default());
                task_kind.push(node.sub_kind.clone().unwrap_or_default());
                task_tier.push(node.tier.clone().unwrap_or_default());
                task_status.push(node.status.clone().unwrap_or_default());
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
    write_parquet(
        &dir.join("spec.parquet"),
        &[
            ("fqn", Col::Str(spec_fqn)),
            ("title", Col::Str(spec_title)),
            ("goal", Col::Str(spec_goal)),
        ],
    )?;
    write_parquet(
        &dir.join("requirement.parquet"),
        &[
            ("fqn", Col::Str(req_fqn)),
            ("id", Col::Str(req_id)),
            ("title", Col::Str(req_title)),
            ("body", Col::Str(req_body)),
            ("feature", Col::Str(req_feature)),
        ],
    )?;
    write_parquet(
        &dir.join("phase.parquet"),
        &[
            ("fqn", Col::Str(phase_fqn)),
            ("number", Col::I64(phase_number)),
            ("title", Col::Str(phase_title)),
        ],
    )?;
    write_parquet(
        &dir.join("decision.parquet"),
        &[
            ("fqn", Col::Str(decision_fqn)),
            ("id", Col::Str(decision_id)),
            ("summary", Col::Str(decision_summary)),
        ],
    )?;
    write_parquet(
        &dir.join("future.parquet"),
        &[
            ("fqn", Col::Str(future_fqn)),
            ("kind", Col::Str(future_kind)),
            ("target", Col::Str(future_target)),
        ],
    )?;
    write_parquet(
        &dir.join("non_goal.parquet"),
        &[("fqn", Col::Str(nongoal_fqn)), ("body", Col::Str(nongoal_body))],
    )?;
    write_parquet(
        &dir.join("acceptance_criterion.parquet"),
        &[("fqn", Col::Str(ac_fqn)), ("body", Col::Str(ac_body))],
    )?;
    write_parquet(
        &dir.join("verification_item.parquet"),
        &[("fqn", Col::Str(vi_fqn)), ("body", Col::Str(vi_body))],
    )?;
    write_parquet(
        &dir.join("note.parquet"),
        &[
            ("fqn", Col::Str(note_fqn)),
            ("body", Col::Str(note_body)),
            ("kind", Col::Str(note_kind)),
        ],
    )?;
    write_parquet(
        &dir.join("feedback.parquet"),
        &[
            ("fqn", Col::Str(feedback_fqn)),
            ("body", Col::Str(feedback_body)),
            ("status", Col::Str(feedback_status)),
            ("disposition", Col::Str(feedback_disposition)),
        ],
    )?;
    write_parquet(
        &dir.join("plan.parquet"),
        &[
            ("fqn", Col::Str(plan_fqn)),
            ("title", Col::Str(plan_title)),
            ("strategy", Col::Str(plan_strategy)),
        ],
    )?;
    write_parquet(
        &dir.join("plan_phase.parquet"),
        &[
            ("fqn", Col::Str(planphase_fqn)),
            ("number", Col::I64(planphase_number)),
            ("title", Col::Str(planphase_title)),
            ("deliverable", Col::Str(planphase_deliverable)),
        ],
    )?;
    write_parquet(
        &dir.join("task.parquet"),
        &[
            ("fqn", Col::Str(task_fqn)),
            ("title", Col::Str(task_title)),
            ("kind", Col::Str(task_kind)),
            ("tier", Col::Str(task_tier)),
            ("status", Col::Str(task_status)),
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
        match (graph.nodes[a].kind, graph.nodes[b].kind) {
            (NodeKind::Module, NodeKind::Module) => {
                c_mm.0.push(a.clone());
                c_mm.1.push(b.clone());
            }
            (NodeKind::Module, NodeKind::File) => {
                c_mfile.0.push(a.clone());
                c_mfile.1.push(b.clone());
            }
            (NodeKind::File, NodeKind::Struct) => {
                c_fs.0.push(a.clone());
                c_fs.1.push(b.clone());
            }
            (NodeKind::File, NodeKind::Function) => {
                c_ff.0.push(a.clone());
                c_ff.1.push(b.clone());
            }
            (NodeKind::Struct, NodeKind::Struct) => {
                c_ss.0.push(a.clone());
                c_ss.1.push(b.clone());
            }
            (NodeKind::Struct, NodeKind::Function) => {
                c_sf.0.push(a.clone());
                c_sf.1.push(b.clone());
            }
            // Spec/plan contains pairs are bucketed below.
            _ => {}
        }
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

    // Spec/plan rel tables, one file per `(from, to)` pair. `contains`
    // (multi-pair) keeps its explicit code pair files; the new tables reuse
    // the pair enumeration so COPY statements stay in sync (SPEC R2/R21).
    let mut contains_spec: HashMap<String, (Vec<String>, Vec<String>)> = HashMap::new();
    for (a, b) in &graph.contains {
        let Some(name) = contains_pair_name(graph.nodes[a].kind, graph.nodes[b].kind) else {
            continue;
        };
        let bucket = contains_spec.entry(name).or_default();
        bucket.0.push(a.clone());
        bucket.1.push(b.clone());
    }
    for (from, to) in contains_pairs() {
        let name = pair_file("contains", from, to);
        let empty = (Vec::new(), Vec::new());
        let (fa, fb) = contains_spec.get(&name).unwrap_or(&empty);
        rel(&name, fa.clone(), fb.clone())?;
    }

    for (table, from, to) in spec_rel_pairs() {
        let mut fa = Vec::new();
        let mut fb = Vec::new();
        match table {
            "Details" => {
                for (a, b) in &graph.details {
                    if graph.nodes[a].kind == from && graph.nodes[b].kind == to {
                        fa.push(a.clone());
                        fb.push(b.clone());
                    }
                }
            }
            "Reviews" => {
                for (a, b) in &graph.reviews {
                    if graph.nodes[a].kind == from && graph.nodes[b].kind == to {
                        fa.push(a.clone());
                        fb.push(b.clone());
                    }
                }
            }
            "Anchors" => {
                for (a, b) in &graph.anchors {
                    if graph.nodes[a].kind == from && graph.nodes[b].kind == to {
                        fa.push(a.clone());
                        fb.push(b.clone());
                    }
                }
            }
            "Implements" => {
                for (a, b) in &graph.implements {
                    if graph.nodes[a].kind == from && graph.nodes[b].kind == to {
                        fa.push(a.clone());
                        fb.push(b.clone());
                    }
                }
            }
            "Gates" => {
                for (a, b) in &graph.gates {
                    if graph.nodes[a].kind == from && graph.nodes[b].kind == to {
                        fa.push(a.clone());
                        fb.push(b.clone());
                    }
                }
            }
            "DependsOn" => {
                for (a, b) in &graph.depends_on {
                    if graph.nodes[a].kind == from && graph.nodes[b].kind == to {
                        fa.push(a.clone());
                        fb.push(b.clone());
                    }
                }
            }
            "SpecDependsOn" => {
                for (a, b) in &graph.spec_depends {
                    if graph.nodes[a].kind == from && graph.nodes[b].kind == to {
                        fa.push(a.clone());
                        fb.push(b.clone());
                    }
                }
            }
            "Satisfies" => {
                for (a, b) in &graph.satisfies {
                    if graph.nodes[a].kind == from && graph.nodes[b].kind == to {
                        fa.push(a.clone());
                        fb.push(b.clone());
                    }
                }
            }
            "Builds" => {
                for (a, b) in &graph.builds {
                    if graph.nodes[a].kind == from && graph.nodes[b].kind == to {
                        fa.push(a.clone());
                        fb.push(b.clone());
                    }
                }
            }
            _ => unreachable!("unknown spec rel table: {table}"),
        }
        rel(&pair_file(table, from, to), fa, fb)?;
    }

    Ok(())
}

/// The `(from, to)` kind pairs of the extended `Contains` table (SPEC §7, R2,
/// R21), with the per-pair PARQUET filename (internal to the load dir).
fn contains_pairs() -> Vec<(NodeKind, NodeKind)> {
    use NodeKind::*;
    vec![
        (Module, Module),
        (Module, File),
        (File, Struct),
        (File, Function),
        (Struct, Struct),
        (Struct, Function),
        (Spec, Requirement),
        (Spec, Phase),
        (Phase, Requirement),
        (Spec, Decision),
        (Spec, NonGoal),
        (Spec, AcceptanceCriterion),
        (Spec, VerificationItem),
        (Plan, PlanPhase),
        (PlanPhase, Task),
        (PlanPhase, AcceptanceCriterion),
        (PlanPhase, VerificationItem),
    ]
}

fn contains_pair_name(a: NodeKind, b: NodeKind) -> Option<String> {
    if contains_pairs().contains(&(a, b)) {
        Some(pair_file("contains", a, b))
    } else {
        None
    }
}

/// The `(from, to)` kind pairs of the spec/plan rel tables (SPEC R2/R21), with
/// the owning table name. Shared by `build_load_files` and `copy_from` so the
/// PARQUET filenames and COPY statements can never drift.
fn spec_rel_pairs() -> Vec<(&'static str, NodeKind, NodeKind)> {
    use NodeKind::*;
    let mut v = Vec::new();
    for to in [
        Module, Function, Struct, File, Spec, Requirement, Phase, Decision, NonGoal,
        AcceptanceCriterion, VerificationItem, Plan, PlanPhase, Task,
    ] {
        v.push(("Details", Note, to));
    }
    for to in [
        Module, Function, Struct, File, Spec, Requirement, Phase, Decision, NonGoal,
        AcceptanceCriterion, VerificationItem, Future, Plan, PlanPhase, Task,
    ] {
        v.push(("Reviews", Feedback, to));
    }
    for to in [Function, Struct, File, Future] {
        v.push(("Anchors", Requirement, to));
    }
    for to in [Function, Struct, File] {
        v.push(("Anchors", Task, to));
    }
    for from in [Function, Struct, File] {
        v.push(("Implements", from, Requirement));
    }
    v.push(("Gates", Phase, Phase));
    v.push(("Gates", PlanPhase, PlanPhase));
    v.push(("DependsOn", Requirement, Requirement));
    v.push(("SpecDependsOn", Spec, Spec));
    v.push(("Satisfies", PlanPhase, Requirement));
    v.push(("Builds", Task, Future));
    v
}

/// Load-file basename for a rel-table `(from, to)` kind pair, e.g.
/// `details_note_module.parquet`. The slug only names the file; the COPY
/// override uses the exact table labels.
fn pair_file(table: &str, from: NodeKind, to: NodeKind) -> String {
    format!("{table}_{}_{}.parquet", kind_slug(from), kind_slug(to))
}

fn kind_slug(k: NodeKind) -> &'static str {
    match k {
        NodeKind::Module => "module",
        NodeKind::Struct => "struct",
        NodeKind::Function => "function",
        NodeKind::File => "file",
        NodeKind::UnresolvedTarget => "unresolved_target",
        NodeKind::Spec => "spec",
        NodeKind::Requirement => "requirement",
        NodeKind::Phase => "phase",
        NodeKind::Decision => "decision",
        NodeKind::Future => "future",
        NodeKind::NonGoal => "non_goal",
        NodeKind::AcceptanceCriterion => "acceptance_criterion",
        NodeKind::VerificationItem => "verification_item",
        NodeKind::Note => "note",
        NodeKind::Feedback => "feedback",
        NodeKind::Plan => "plan",
        NodeKind::PlanPhase => "plan_phase",
        NodeKind::Task => "task",
    }
}

/// The exact node-table label used in `CREATE REL TABLE` and COPY overrides.
fn label_of(k: NodeKind) -> &'static str {
    match k {
        NodeKind::Module => "Module",
        NodeKind::Struct => "Struct",
        NodeKind::Function => "Function",
        NodeKind::File => "File",
        NodeKind::UnresolvedTarget => "UnresolvedTarget",
        NodeKind::Spec => "Spec",
        NodeKind::Requirement => "Requirement",
        NodeKind::Phase => "Phase",
        NodeKind::Decision => "Decision",
        NodeKind::Future => "Future",
        NodeKind::NonGoal => "NonGoal",
        NodeKind::AcceptanceCriterion => "AcceptanceCriterion",
        NodeKind::VerificationItem => "VerificationItem",
        NodeKind::Note => "Note",
        NodeKind::Feedback => "Feedback",
        NodeKind::Plan => "Plan",
        NodeKind::PlanPhase => "PlanPhase",
        NodeKind::Task => "Task",
    }
}

/// Creates the LadybugDB schema (SPEC §7, R1/R2/R20/R21): the five code node
/// tables, the thirteen spec/plan node tables, and the rel tables (Contains
/// extended with spec/plan pairs, plus the nine spec/plan rel tables).
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
        "CREATE NODE TABLE Spec(fqn STRING PRIMARY KEY, title STRING, goal STRING)",
    )?;
    conn.query(
        "CREATE NODE TABLE Requirement(fqn STRING PRIMARY KEY, id STRING, title STRING, body STRING, feature STRING)",
    )?;
    conn.query(
        "CREATE NODE TABLE Phase(fqn STRING PRIMARY KEY, number INT64, title STRING)",
    )?;
    conn.query(
        "CREATE NODE TABLE Decision(fqn STRING PRIMARY KEY, id STRING, summary STRING)",
    )?;
    conn.query(
        "CREATE NODE TABLE Future(fqn STRING PRIMARY KEY, kind STRING, target STRING)",
    )?;
    conn.query("CREATE NODE TABLE NonGoal(fqn STRING PRIMARY KEY, body STRING)")?;
    conn.query(
        "CREATE NODE TABLE AcceptanceCriterion(fqn STRING PRIMARY KEY, body STRING)",
    )?;
    conn.query(
        "CREATE NODE TABLE VerificationItem(fqn STRING PRIMARY KEY, body STRING)",
    )?;
    conn.query("CREATE NODE TABLE Note(fqn STRING PRIMARY KEY, body STRING, kind STRING)")?;
    conn.query(
        "CREATE NODE TABLE Feedback(fqn STRING PRIMARY KEY, body STRING, status STRING, disposition STRING)",
    )?;
    conn.query(
        "CREATE NODE TABLE Plan(fqn STRING PRIMARY KEY, title STRING, strategy STRING)",
    )?;
    conn.query(
        "CREATE NODE TABLE PlanPhase(fqn STRING PRIMARY KEY, number INT64, title STRING, deliverable STRING)",
    )?;
    conn.query(
        "CREATE NODE TABLE Task(fqn STRING PRIMARY KEY, title STRING, kind STRING, tier STRING, status STRING)",
    )?;
    conn.query(
        "CREATE REL TABLE Contains(FROM Module TO Module, FROM Module TO File, FROM File TO Struct, FROM File TO Function, FROM Struct TO Struct, FROM Struct TO Function, FROM Spec TO Requirement, FROM Spec TO Phase, FROM Phase TO Requirement, FROM Spec TO Decision, FROM Spec TO NonGoal, FROM Spec TO AcceptanceCriterion, FROM Spec TO VerificationItem, FROM Plan TO PlanPhase, FROM PlanPhase TO Task, FROM PlanPhase TO AcceptanceCriterion, FROM PlanPhase TO VerificationItem)",
    )?;
    conn.query("CREATE REL TABLE Calls(FROM Function TO Function)")?;
    conn.query("CREATE REL TABLE Uses(FROM Function TO Struct, FROM Struct TO Struct)")?;
    conn.query(
        "CREATE REL TABLE UnresolvedCall(FROM Function TO UnresolvedTarget, target_type STRING)",
    )?;
    conn.query(
        "CREATE REL TABLE UnresolvedUse(FROM Function TO UnresolvedTarget, FROM Struct TO UnresolvedTarget)",
    )?;
    conn.query(
        "CREATE REL TABLE Details(FROM Note TO Module, FROM Note TO Function, FROM Note TO Struct, FROM Note TO File, FROM Note TO Spec, FROM Note TO Requirement, FROM Note TO Phase, FROM Note TO Decision, FROM Note TO NonGoal, FROM Note TO AcceptanceCriterion, FROM Note TO VerificationItem, FROM Note TO Plan, FROM Note TO PlanPhase, FROM Note TO Task)",
    )?;
    conn.query(
        "CREATE REL TABLE Reviews(FROM Feedback TO Module, FROM Feedback TO Function, FROM Feedback TO Struct, FROM Feedback TO File, FROM Feedback TO Spec, FROM Feedback TO Requirement, FROM Feedback TO Phase, FROM Feedback TO Decision, FROM Feedback TO NonGoal, FROM Feedback TO AcceptanceCriterion, FROM Feedback TO VerificationItem, FROM Feedback TO Future, FROM Feedback TO Plan, FROM Feedback TO PlanPhase, FROM Feedback TO Task)",
    )?;
    conn.query("CREATE REL TABLE DependsOn(FROM Requirement TO Requirement)")?;
    conn.query(
        "CREATE REL TABLE Gates(FROM Phase TO Phase, FROM PlanPhase TO PlanPhase)",
    )?;
    conn.query("CREATE REL TABLE SpecDependsOn(FROM Spec TO Spec)")?;
    conn.query(
        "CREATE REL TABLE Anchors(FROM Requirement TO Function, FROM Requirement TO Struct, FROM Requirement TO File, FROM Requirement TO Future, FROM Task TO Function, FROM Task TO Struct, FROM Task TO File)",
    )?;
    conn.query(
        "CREATE REL TABLE Implements(FROM Function TO Requirement, FROM Struct TO Requirement, FROM File TO Requirement)",
    )?;
    conn.query("CREATE REL TABLE Satisfies(FROM PlanPhase TO Requirement)")?;
    conn.query("CREATE REL TABLE Builds(FROM Task TO Future)")?;
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
        format!(r#"COPY Spec FROM "{}""#, p("spec.parquet")),
        format!(r#"COPY Requirement FROM "{}""#, p("requirement.parquet")),
        format!(r#"COPY Phase FROM "{}""#, p("phase.parquet")),
        format!(r#"COPY Decision FROM "{}""#, p("decision.parquet")),
        format!(r#"COPY Future FROM "{}""#, p("future.parquet")),
        format!(r#"COPY NonGoal FROM "{}""#, p("non_goal.parquet")),
        format!(
            r#"COPY AcceptanceCriterion FROM "{}""#,
            p("acceptance_criterion.parquet")
        ),
        format!(
            r#"COPY VerificationItem FROM "{}""#,
            p("verification_item.parquet")
        ),
        format!(r#"COPY Note FROM "{}""#, p("note.parquet")),
        format!(r#"COPY Feedback FROM "{}""#, p("feedback.parquet")),
        format!(r#"COPY Plan FROM "{}""#, p("plan.parquet")),
        format!(r#"COPY PlanPhase FROM "{}""#, p("plan_phase.parquet")),
        format!(r#"COPY Task FROM "{}""#, p("task.parquet")),
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
    // Spec/plan rel tables: one COPY per `(from, to)` pair, generated from the
    // same pair enumeration that wrote the files.
    let mut contains_stmt = Vec::new();
    for (from, to) in contains_pairs() {
        let name = pair_file("contains", from, to);
        if [
            (NodeKind::Module, NodeKind::Module),
            (NodeKind::Module, NodeKind::File),
            (NodeKind::File, NodeKind::Struct),
            (NodeKind::File, NodeKind::Function),
            (NodeKind::Struct, NodeKind::Struct),
            (NodeKind::Struct, NodeKind::Function),
        ]
        .contains(&(from, to))
        {
            continue;
        }
        contains_stmt.push(format!(
            r#"COPY Contains FROM "{}" (from="{}", to="{}")"#,
            p(&name),
            label_of(from),
            label_of(to)
        ));
    }
    for s in contains_stmt {
        conn.query(&s)?;
    }
    for (table, from, to) in spec_rel_pairs() {
        let name = pair_file(table, from, to);
        conn.query(&format!(
            r#"COPY {table} FROM "{}" (from="{}", to="{}")"#,
            p(&name),
            label_of(from),
            label_of(to)
        ))?;
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
    Spec {
        fqn: String,
        title: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        goal: String,
    },
    Requirement {
        fqn: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        id: String,
        title: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        body: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        feature: String,
    },
    Phase {
        fqn: String,
        number: u32,
        title: String,
    },
    Decision {
        fqn: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        id: String,
        summary: String,
    },
    Future {
        fqn: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        kind: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        target: String,
    },
    NonGoal {
        fqn: String,
        body: String,
    },
    AcceptanceCriterion {
        fqn: String,
        body: String,
    },
    VerificationItem {
        fqn: String,
        body: String,
    },
    Note {
        fqn: String,
        body: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        kind: String,
    },
    Feedback {
        fqn: String,
        body: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        status: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        disposition: String,
    },
    Plan {
        fqn: String,
        title: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        strategy: String,
    },
    PlanPhase {
        fqn: String,
        number: u32,
        title: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        deliverable: String,
    },
    Task {
        fqn: String,
        title: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        kind: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        tier: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        status: String,
    },
    Details {
        from: String,
        to: String,
    },
    Reviews {
        from: String,
        to: String,
    },
    DependsOn {
        from: String,
        to: String,
    },
    Gates {
        from: String,
        to: String,
    },
    SpecDepends {
        from: String,
        to: String,
    },
    Anchors {
        from: String,
        to: String,
    },
    Implements {
        from: String,
        to: String,
    },
    Satisfies {
        from: String,
        to: String,
    },
    Builds {
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
            NodeKind::Spec => Export::Spec {
                fqn: fqn.clone(),
                title: node.title.clone().unwrap_or_default(),
                goal: node.goal.clone().unwrap_or_default(),
            },
            NodeKind::Requirement => Export::Requirement {
                fqn: fqn.clone(),
                id: node.id.clone().unwrap_or_default(),
                title: node.title.clone().unwrap_or_default(),
                body: node.body.clone().unwrap_or_default(),
                feature: node.feature.clone().unwrap_or_default(),
            },
            NodeKind::Phase => Export::Phase {
                fqn: fqn.clone(),
                number: node.number.unwrap_or_default(),
                title: node.title.clone().unwrap_or_default(),
            },
            NodeKind::Decision => Export::Decision {
                fqn: fqn.clone(),
                id: node.id.clone().unwrap_or_default(),
                summary: node.summary.clone().unwrap_or_default(),
            },
            NodeKind::Future => Export::Future {
                fqn: fqn.clone(),
                kind: node.sub_kind.clone().unwrap_or_default(),
                target: node.target.clone().unwrap_or_default(),
            },
            NodeKind::NonGoal => Export::NonGoal {
                fqn: fqn.clone(),
                body: node.body.clone().unwrap_or_default(),
            },
            NodeKind::AcceptanceCriterion => Export::AcceptanceCriterion {
                fqn: fqn.clone(),
                body: node.body.clone().unwrap_or_default(),
            },
            NodeKind::VerificationItem => Export::VerificationItem {
                fqn: fqn.clone(),
                body: node.body.clone().unwrap_or_default(),
            },
            NodeKind::Note => Export::Note {
                fqn: fqn.clone(),
                body: node.body.clone().unwrap_or_default(),
                kind: node.sub_kind.clone().unwrap_or_default(),
            },
            NodeKind::Feedback => Export::Feedback {
                fqn: fqn.clone(),
                body: node.body.clone().unwrap_or_default(),
                status: node.status.clone().unwrap_or_default(),
                disposition: node.disposition.clone().unwrap_or_default(),
            },
            NodeKind::Plan => Export::Plan {
                fqn: fqn.clone(),
                title: node.title.clone().unwrap_or_default(),
                strategy: node.strategy.clone().unwrap_or_default(),
            },
            NodeKind::PlanPhase => Export::PlanPhase {
                fqn: fqn.clone(),
                number: node.number.unwrap_or_default(),
                title: node.title.clone().unwrap_or_default(),
                deliverable: node.deliverable.clone().unwrap_or_default(),
            },
            NodeKind::Task => Export::Task {
                fqn: fqn.clone(),
                title: node.title.clone().unwrap_or_default(),
                kind: node.sub_kind.clone().unwrap_or_default(),
                tier: node.tier.clone().unwrap_or_default(),
                status: node.status.clone().unwrap_or_default(),
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
    for (a, b) in &graph.details {
        write_line(
            &mut w,
            &Export::Details {
                from: a.clone(),
                to: b.clone(),
            },
        )?;
    }
    for (a, b) in &graph.reviews {
        write_line(
            &mut w,
            &Export::Reviews {
                from: a.clone(),
                to: b.clone(),
            },
        )?;
    }
    for (a, b) in &graph.depends_on {
        write_line(
            &mut w,
            &Export::DependsOn {
                from: a.clone(),
                to: b.clone(),
            },
        )?;
    }
    for (a, b) in &graph.gates {
        write_line(
            &mut w,
            &Export::Gates {
                from: a.clone(),
                to: b.clone(),
            },
        )?;
    }
    for (a, b) in &graph.spec_depends {
        write_line(
            &mut w,
            &Export::SpecDepends {
                from: a.clone(),
                to: b.clone(),
            },
        )?;
    }
    for (a, b) in &graph.anchors {
        write_line(
            &mut w,
            &Export::Anchors {
                from: a.clone(),
                to: b.clone(),
            },
        )?;
    }
    for (a, b) in &graph.implements {
        write_line(
            &mut w,
            &Export::Implements {
                from: a.clone(),
                to: b.clone(),
            },
        )?;
    }
    for (a, b) in &graph.satisfies {
        write_line(
            &mut w,
            &Export::Satisfies {
                from: a.clone(),
                to: b.clone(),
            },
        )?;
    }
    for (a, b) in &graph.builds {
        write_line(
            &mut w,
            &Export::Builds {
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
            ..Node::default()
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
    fn spec_plan_schema_roundtrip() {
        // A spec + plan graph (SPEC R1/R2/R20/R21) survives the PARQUET load
        // path: node tables with their own columns, multi-pair Contains, and
        // the nine spec/plan rel tables (one COPY per pair, empty pairs too).
        let mut g = Graph::default();
        let sp = |kind: NodeKind| Node {
            kind,
            code_type: String::new(),
            ..Node::default()
        };
        let mut n = |fqn: &str, node: Node| {
            g.nodes.insert(fqn.to_string(), node);
        };
        n(
            "future/foo/spec",
            Node {
                title: Some("Widget timer".to_string()),
                goal: Some("Let widgets time out".to_string()),
                ..sp(NodeKind::Spec)
            },
        );
        n(
            "future/foo/spec.R1",
            Node {
                id: Some("R1".to_string()),
                title: Some("Timer".to_string()),
                feature: Some("feature-a".to_string()),
                ..sp(NodeKind::Requirement)
            },
        );
        n(
            "future/foo/spec.R2",
            Node {
                id: Some("R2".to_string()),
                title: Some("Expiry".to_string()),
                ..sp(NodeKind::Requirement)
            },
        );
        n(
            "future/foo/spec.phase-1",
            Node {
                number: Some(1),
                title: Some("Core".to_string()),
                ..sp(NodeKind::Phase)
            },
        );
        n(
            "future/foo/spec.phase-2",
            Node {
                number: Some(2),
                title: Some("Polish".to_string()),
                ..sp(NodeKind::Phase)
            },
        );
        n(
            "future/foo/spec.decision-d1",
            Node {
                id: Some("d1".to_string()),
                summary: Some("Timeout in wall clock".to_string()),
                ..sp(NodeKind::Decision)
            },
        );
        n(
            "future/foo/spec.ng1",
            Node {
                body: Some("No daemon".to_string()),
                ..sp(NodeKind::NonGoal)
            },
        );
        n(
            "future/foo/spec.ac1",
            Node {
                body: Some("Timer fires once".to_string()),
                ..sp(NodeKind::AcceptanceCriterion)
            },
        );
        n(
            "future/foo/spec.vi1",
            Node {
                body: Some("cargo test green".to_string()),
                ..sp(NodeKind::VerificationItem)
            },
        );
        n(
            "future/foo/gateway",
            Node {
                sub_kind: Some("rpc".to_string()),
                target: Some("github.com/x/gateway".to_string()),
                ..sp(NodeKind::Future)
            },
        );
        n(
            "future/foo/note-1",
            Node {
                body: Some("Background prose".to_string()),
                sub_kind: Some("background".to_string()),
                ..sp(NodeKind::Note)
            },
        );
        n(
            "future/other/spec",
            Node {
                title: Some("Other".to_string()),
                ..sp(NodeKind::Spec)
            },
        );
        n(
            "future/foo/feedback-1",
            Node {
                body: Some("Split R1".to_string()),
                status: Some("open".to_string()),
                ..sp(NodeKind::Feedback)
            },
        );
        n(
            "future/foo/plan",
            Node {
                title: Some("Plan".to_string()),
                strategy: Some("Layer-first".to_string()),
                ..sp(NodeKind::Plan)
            },
        );
        n(
            "future/foo/plan.phase-1",
            Node {
                number: Some(1),
                title: Some("P1".to_string()),
                deliverable: Some("Core".to_string()),
                ..sp(NodeKind::PlanPhase)
            },
        );
        n(
            "future/foo/plan.phase-1.task-1",
            Node {
                title: Some("Add RootStore".to_string()),
                sub_kind: Some("source".to_string()),
                tier: Some("".to_string()),
                status: Some("pending".to_string()),
                ..sp(NodeKind::Task)
            },
        );

        g.contains.extend([
            ("future/foo/spec".into(), "future/foo/spec.R1".into()),
            ("future/foo/spec".into(), "future/foo/spec.phase-1".into()),
            ("future/foo/spec".into(), "future/foo/spec.phase-2".into()),
            ("future/foo/spec".into(), "future/foo/spec.decision-d1".into()),
            ("future/foo/spec".into(), "future/foo/spec.ng1".into()),
            ("future/foo/spec".into(), "future/foo/spec.ac1".into()),
            ("future/foo/spec".into(), "future/foo/spec.vi1".into()),
            ("future/foo/spec.phase-1".into(), "future/foo/spec.R1".into()),
            ("future/foo/plan".into(), "future/foo/plan.phase-1".into()),
            (
                "future/foo/plan.phase-1".into(),
                "future/foo/plan.phase-1.task-1".into(),
            ),
        ]);
        g.details
            .insert(("future/foo/note-1".into(), "future/foo/spec".into()));
        g.reviews.insert((
            "future/foo/feedback-1".into(),
            "future/foo/spec.R1".into(),
        ));
        g.depends_on
            .insert(("future/foo/spec.R2".into(), "future/foo/spec.R1".into()));
        g.gates.insert((
            "future/foo/spec.phase-2".into(),
            "future/foo/spec.phase-1".into(),
        ));
        g.spec_depends
            .insert(("future/foo/spec".into(), "future/other/spec".into()));
        g.anchors.insert((
            "future/foo/spec.R1".into(),
            "future/foo/gateway".into(),
        ));
        g.satisfies.insert((
            "future/foo/plan.phase-1".into(),
            "future/foo/spec.R1".into(),
        ));
        g.builds.insert((
            "future/foo/plan.phase-1.task-1".into(),
            "future/foo/gateway".into(),
        ));

        let dir = std::env::temp_dir().join(format!("apg-test-spec-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        build_load_files(&g, &dir).unwrap();

        let db = Database::in_memory(SystemConfig::default()).unwrap();
        let conn = Connection::new(&db).unwrap();
        create_schema(&conn).unwrap();
        copy_from(&conn, &dir).unwrap();

        // Node tables carry their own columns.
        let out = conn
            .query("MATCH (s:Spec) RETURN s.fqn, s.title, s.goal")
            .unwrap()
            .to_string();
        assert!(
            out.contains("future/foo/spec") && out.contains("Widget timer"),
            "spec rows: {out}"
        );
        let out = conn
            .query("MATCH (r:Requirement) RETURN r.id, r.feature")
            .unwrap()
            .to_string();
        assert!(out.contains("R1") && out.contains("feature-a"), "req rows: {out}");
        let out = conn
            .query("MATCH (p:PlanPhase) RETURN p.number, p.deliverable")
            .unwrap()
            .to_string();
        assert!(out.contains("1") && out.contains("Core"), "plan phase rows: {out}");
        let out = conn
            .query("MATCH (t:Task) RETURN t.kind, t.tier, t.status")
            .unwrap()
            .to_string();
        assert!(out.contains("source") && out.contains("pending"), "task rows: {out}");
        let out = conn
            .query("MATCH (f:Future) RETURN f.kind, f.target")
            .unwrap()
            .to_string();
        assert!(
            out.contains("rpc") && out.contains("github.com/x/gateway"),
            "future rows: {out}"
        );

        // Multi-pair Contains: Spec -> Requirement and Spec -> Phase.
        let out = conn
            .query("MATCH (s:Spec)-[:Contains]->(r:Requirement) RETURN r.fqn")
            .unwrap()
            .to_string();
        assert!(out.contains("future/foo/spec.R1"), "contains spec->req: {out}");
        let out = conn
            .query("MATCH (s:Spec)-[:Contains]->(p:Phase) RETURN p.fqn")
            .unwrap()
            .to_string();
        assert!(
            out.contains("future/foo/spec.phase-1"),
            "contains spec->phase: {out}"
        );

        // Spec/plan rel tables.
        let out = conn
            .query("MATCH (:Note)-[:Details]->(s:Spec) RETURN s.fqn")
            .unwrap()
            .to_string();
        assert!(out.contains("future/foo/spec"), "details: {out}");
        let out = conn
            .query("MATCH (:Feedback)-[:Reviews]->(r:Requirement) RETURN r.fqn")
            .unwrap()
            .to_string();
        assert!(out.contains("future/foo/spec.R1"), "reviews: {out}");
        let out = conn
            .query("MATCH (a:Requirement)-[:DependsOn]->(b:Requirement) RETURN b.fqn")
            .unwrap()
            .to_string();
        assert!(out.contains("future/foo/spec.R1"), "depends_on: {out}");
        let out = conn
            .query("MATCH (a:Phase)-[:Gates]->(b:Phase) RETURN b.fqn")
            .unwrap()
            .to_string();
        assert!(out.contains("future/foo/spec.phase-1"), "gates: {out}");
        let out = conn
            .query("MATCH (a:Requirement)-[:Anchors]->(f:Future) RETURN f.fqn")
            .unwrap()
            .to_string();
        assert!(out.contains("future/foo/gateway"), "anchors: {out}");
        let out = conn
            .query("MATCH (:PlanPhase)-[:Satisfies]->(r:Requirement) RETURN r.fqn")
            .unwrap()
            .to_string();
        assert!(out.contains("future/foo/spec.R1"), "satisfies: {out}");
        let out = conn
            .query("MATCH (:Task)-[:Builds]->(f:Future) RETURN f.fqn")
            .unwrap()
            .to_string();
        assert!(out.contains("future/foo/gateway"), "builds: {out}");

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
