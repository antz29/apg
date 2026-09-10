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
    let mut module_status = Vec::new();
    let mut scan_fqn = Vec::new();
    let mut scan_git_sha = Vec::new();
    let mut scan_git_clean = Vec::new();
    let mut scan_scanned_at = Vec::new();
    let mut struct_fqn = Vec::new();
    let mut struct_path = Vec::new();
    let mut struct_start = Vec::new();
    let mut struct_end = Vec::new();
    let mut struct_start_line = Vec::new();
    let mut struct_end_line = Vec::new();
    let mut struct_ct = Vec::new();
    let mut struct_status = Vec::new();
    let mut fn_fqn = Vec::new();
    let mut fn_path = Vec::new();
    let mut fn_start = Vec::new();
    let mut fn_end = Vec::new();
    let mut fn_start_line = Vec::new();
    let mut fn_end_line = Vec::new();
    let mut fn_ct = Vec::new();
    let mut fn_status = Vec::new();
    let mut file_fqn = Vec::new();
    let mut file_start_line = Vec::new();
    let mut file_end_line = Vec::new();
    let mut file_ct = Vec::new();
    let mut file_status = Vec::new();
    let mut unres_fqn = Vec::new();
    let mut unres_cat = Vec::new();
    let mut req_fqn = Vec::new();
    let mut req_id = Vec::new();
    let mut req_title = Vec::new();
    let mut req_body = Vec::new();
    let mut req_feature = Vec::new();
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
    let mut planphase_status = Vec::new();
    let mut task_fqn = Vec::new();
    let mut task_title = Vec::new();
    let mut task_kind = Vec::new();
    let mut task_tier = Vec::new();
    let mut task_status = Vec::new();

    // Tier-1/2/3 node tables (GraphModel-SPEC.md; PHASE_01).
    let mut stakeholder_fqn = Vec::new();
    let mut stakeholder_name = Vec::new();
    let mut stakeholder_body = Vec::new();
    let mut entity_fqn = Vec::new();
    let mut entity_name = Vec::new();
    let mut entity_body = Vec::new();
    let mut system_fqn = Vec::new();
    let mut system_name = Vec::new();
    let mut system_body = Vec::new();
    let mut container_fqn = Vec::new();
    let mut container_name = Vec::new();
    let mut container_kind = Vec::new();
    let mut container_body = Vec::new();
    let mut component_fqn = Vec::new();
    let mut component_name = Vec::new();
    let mut component_body = Vec::new();

    // New-model tier catalog (apg-projects SPEC §3.1).
    let mut user_fqn = Vec::new();
    let mut user_name = Vec::new();
    let mut user_body = Vec::new();
    let mut group_fqn = Vec::new();
    let mut group_name = Vec::new();
    let mut group_attribute = Vec::new();
    let mut group_root = Vec::new();
    let mut group_body = Vec::new();
    let mut value_fqn = Vec::new();
    let mut value_name = Vec::new();
    let mut value_body = Vec::new();
    let mut service_fqn = Vec::new();
    let mut service_name = Vec::new();
    let mut service_body = Vec::new();
    let mut person_fqn = Vec::new();
    let mut person_name = Vec::new();
    let mut person_body = Vec::new();
    let mut constraint_fqn = Vec::new();
    let mut constraint_name = Vec::new();
    let mut constraint_body = Vec::new();
    let mut constraint_attaches_to = Vec::new();

    for (fqn, node) in &graph.nodes {
        match node.kind {
            NodeKind::Module => {
                module_fqn.push(fqn.clone());
                module_status.push(node.status.clone().unwrap_or_default());
            }
            NodeKind::Scan => {
                scan_fqn.push(fqn.clone());
                scan_git_sha.push(node.git_sha.clone().unwrap_or_default());
                // The Scan table stores `git_clean` as a STRING ("true"/"false",
                // empty when not a git repo) — the load path's parquet writer
                // has STRING/INT64 columns only.
                scan_git_clean.push(node.git_clean.map(|c| c.to_string()).unwrap_or_default());
                scan_scanned_at.push(node.scanned_at.clone().unwrap_or_default());
            }
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
                struct_status.push(node.status.clone().unwrap_or_default());
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
                fn_status.push(node.status.clone().unwrap_or_default());
            }
            NodeKind::File => {
                file_fqn.push(fqn.clone());
                let (sl, el) = lines(graph, fqn);
                file_start_line.push(sl);
                file_end_line.push(el);
                file_ct.push(node.code_type.clone());
                file_status.push(node.status.clone().unwrap_or_default());
            }
            NodeKind::UnresolvedTarget => {
                unres_fqn.push(fqn.clone());
                unres_cat.push(node.category.clone().unwrap_or_default());
            }
            NodeKind::Requirement => {
                req_fqn.push(fqn.clone());
                req_id.push(node.id.clone().unwrap_or_default());
                req_title.push(node.title.clone().unwrap_or_default());
                req_body.push(node.body.clone().unwrap_or_default());
                req_feature.push(node.feature.clone().unwrap_or_default());
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
                planphase_status.push(node.status.clone().unwrap_or_default());
            }
            NodeKind::Task => {
                task_fqn.push(fqn.clone());
                task_title.push(node.title.clone().unwrap_or_default());
                task_kind.push(node.sub_kind.clone().unwrap_or_default());
                task_tier.push(node.tier.clone().unwrap_or_default());
                task_status.push(node.status.clone().unwrap_or_default());
            }
            NodeKind::Stakeholder => {
                stakeholder_fqn.push(fqn.clone());
                stakeholder_name.push(node.name.clone().unwrap_or_default());
                stakeholder_body.push(node.body.clone().unwrap_or_default());
            }
            NodeKind::Entity => {
                entity_fqn.push(fqn.clone());
                entity_name.push(node.name.clone().unwrap_or_default());
                entity_body.push(node.body.clone().unwrap_or_default());
            }
            NodeKind::System => {
                system_fqn.push(fqn.clone());
                system_name.push(node.name.clone().unwrap_or_default());
                system_body.push(node.body.clone().unwrap_or_default());
            }
            NodeKind::Container => {
                container_fqn.push(fqn.clone());
                container_name.push(node.name.clone().unwrap_or_default());
                container_kind.push(node.sub_kind.clone().unwrap_or_default());
                container_body.push(node.body.clone().unwrap_or_default());
            }
            NodeKind::Component => {
                component_fqn.push(fqn.clone());
                component_name.push(node.name.clone().unwrap_or_default());
                component_body.push(node.body.clone().unwrap_or_default());
            }
            NodeKind::User => {
                user_fqn.push(fqn.clone());
                user_name.push(node.name.clone().unwrap_or_default());
                user_body.push(node.body.clone().unwrap_or_default());
            }
            NodeKind::Group => {
                group_fqn.push(fqn.clone());
                group_name.push(node.name.clone().unwrap_or_default());
                group_attribute.push(node.attribute.clone().unwrap_or_default());
                group_root.push(node.root.clone().unwrap_or_default());
                group_body.push(node.body.clone().unwrap_or_default());
            }
            NodeKind::Value => {
                value_fqn.push(fqn.clone());
                value_name.push(node.name.clone().unwrap_or_default());
                value_body.push(node.body.clone().unwrap_or_default());
            }
            NodeKind::Service => {
                service_fqn.push(fqn.clone());
                service_name.push(node.name.clone().unwrap_or_default());
                service_body.push(node.body.clone().unwrap_or_default());
            }
            NodeKind::Person => {
                person_fqn.push(fqn.clone());
                person_name.push(node.name.clone().unwrap_or_default());
                person_body.push(node.body.clone().unwrap_or_default());
            }
            NodeKind::Constraint => {
                constraint_fqn.push(fqn.clone());
                constraint_name.push(node.name.clone().unwrap_or_default());
                constraint_body.push(node.body.clone().unwrap_or_default());
                constraint_attaches_to.push(node.attaches_to.clone().unwrap_or_default());
            }
        }
    }

    write_parquet(
        &dir.join("module.parquet"),
        &[
            ("fqn", Col::Str(module_fqn)),
            ("status", Col::Str(module_status)),
        ],
    )?;
    write_parquet(
        &dir.join("scan.parquet"),
        &[
            ("fqn", Col::Str(scan_fqn)),
            ("git_sha", Col::Str(scan_git_sha)),
            ("git_clean", Col::Str(scan_git_clean)),
            ("scanned_at", Col::Str(scan_scanned_at)),
        ],
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
            ("status", Col::Str(struct_status)),
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
            ("status", Col::Str(fn_status)),
        ],
    )?;
    write_parquet(
        &dir.join("file.parquet"),
        &[
            ("fqn", Col::Str(file_fqn)),
            ("start_line", Col::I64(file_start_line)),
            ("end_line", Col::I64(file_end_line)),
            ("code_type", Col::Str(file_ct)),
            ("status", Col::Str(file_status)),
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
            ("status", Col::Str(planphase_status)),
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
    write_parquet(
        &dir.join("stakeholder.parquet"),
        &[
            ("fqn", Col::Str(stakeholder_fqn)),
            ("name", Col::Str(stakeholder_name)),
            ("body", Col::Str(stakeholder_body)),
        ],
    )?;
    write_parquet(
        &dir.join("entity.parquet"),
        &[
            ("fqn", Col::Str(entity_fqn)),
            ("name", Col::Str(entity_name)),
            ("body", Col::Str(entity_body)),
        ],
    )?;
    write_parquet(
        &dir.join("system.parquet"),
        &[
            ("fqn", Col::Str(system_fqn)),
            ("name", Col::Str(system_name)),
            ("body", Col::Str(system_body)),
        ],
    )?;
    write_parquet(
        &dir.join("container.parquet"),
        &[
            ("fqn", Col::Str(container_fqn)),
            ("name", Col::Str(container_name)),
            ("kind", Col::Str(container_kind)),
            ("body", Col::Str(container_body)),
        ],
    )?;
    write_parquet(
        &dir.join("component.parquet"),
        &[
            ("fqn", Col::Str(component_fqn)),
            ("name", Col::Str(component_name)),
            ("body", Col::Str(component_body)),
        ],
    )?;
    write_parquet(
        &dir.join("user.parquet"),
        &[
            ("fqn", Col::Str(user_fqn)),
            ("name", Col::Str(user_name)),
            ("body", Col::Str(user_body)),
        ],
    )?;
    write_parquet(
        &dir.join("group.parquet"),
        &[
            ("fqn", Col::Str(group_fqn)),
            ("name", Col::Str(group_name)),
            ("attribute", Col::Str(group_attribute)),
            ("root", Col::Str(group_root)),
            ("body", Col::Str(group_body)),
        ],
    )?;
    write_parquet(
        &dir.join("value.parquet"),
        &[
            ("fqn", Col::Str(value_fqn)),
            ("name", Col::Str(value_name)),
            ("body", Col::Str(value_body)),
        ],
    )?;
    write_parquet(
        &dir.join("service.parquet"),
        &[
            ("fqn", Col::Str(service_fqn)),
            ("name", Col::Str(service_name)),
            ("body", Col::Str(service_body)),
        ],
    )?;
    write_parquet(
        &dir.join("person.parquet"),
        &[
            ("fqn", Col::Str(person_fqn)),
            ("name", Col::Str(person_name)),
            ("body", Col::Str(person_body)),
        ],
    )?;
    write_parquet(
        &dir.join("constraint.parquet"),
        &[
            ("fqn", Col::Str(constraint_fqn)),
            ("name", Col::Str(constraint_name)),
            ("body", Col::Str(constraint_body)),
            ("attaches_to", Col::Str(constraint_attaches_to)),
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

    let mut calls_fn = (Vec::new(), Vec::new());
    let mut calls_svc = (Vec::new(), Vec::new());
    for (a, b) in &graph.calls {
        let dst = match graph.nodes[a].kind {
            NodeKind::Function => &mut calls_fn,
            NodeKind::Service => &mut calls_svc,
            _ => unreachable!("unvalidated calls edge"),
        };
        dst.0.push(a.clone());
        dst.1.push(b.clone());
    }

    let mut u_fn = (Vec::new(), Vec::new());
    let mut u_st = (Vec::new(), Vec::new());
    let mut u_person = (Vec::new(), Vec::new());
    for (a, b) in &graph.uses {
        let dst = match graph.nodes[a].kind {
            NodeKind::Function => &mut u_fn,
            NodeKind::Struct => &mut u_st,
            NodeKind::Person => &mut u_person,
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
    rel("calls_fn.parquet", calls_fn.0, calls_fn.1)?;
    rel("calls_svc.parquet", calls_svc.0, calls_svc.1)?;
    rel("uses_fn.parquet", u_fn.0, u_fn.1)?;
    rel("uses_struct.parquet", u_st.0, u_st.1)?;
    rel("uses_person.parquet", u_person.0, u_person.1)?;
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
            "Satisfies" => {
                for (a, b) in &graph.satisfies {
                    if graph.nodes[a].kind == from && graph.nodes[b].kind == to {
                        fa.push(a.clone());
                        fb.push(b.clone());
                    }
                }
            }
            "Drives" => {
                for (a, b) in &graph.drives {
                    if graph.nodes[a].kind == from && graph.nodes[b].kind == to {
                        fa.push(a.clone());
                        fb.push(b.clone());
                    }
                }
            }
            "Represents" => {
                for (a, b) in &graph.represents {
                    if graph.nodes[a].kind == from && graph.nodes[b].kind == to {
                        fa.push(a.clone());
                        fb.push(b.clone());
                    }
                }
            }
            // New-model §3.3 spec edges (apg-projects).
            "RealisedBy" => {
                for (a, b) in &graph.realised_by {
                    if graph.nodes[a].kind == from && graph.nodes[b].kind == to {
                        fa.push(a.clone());
                        fb.push(b.clone());
                    }
                }
            }
            "SpecImplementedBy" => {
                for (a, b) in &graph.spec_implemented_by {
                    if graph.nodes[a].kind == from && graph.nodes[b].kind == to {
                        fa.push(a.clone());
                        fb.push(b.clone());
                    }
                }
            }
            "Publishes" => {
                for (a, b) in &graph.publishes {
                    if graph.nodes[a].kind == from && graph.nodes[b].kind == to {
                        fa.push(a.clone());
                        fb.push(b.clone());
                    }
                }
            }
            "Subscribes" => {
                for (a, b) in &graph.subscribes {
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
        (Plan, PlanPhase),
        (PlanPhase, Task),
        // New-model §3.3 `contains` rows (apg-projects): the requirements tree
        // (Stakeholder/User/Requirement ⊃ Requirement), the plain-named domain
        // hierarchy (Group ⊃ Group/Entity/Value/Service), and the solution
        // hierarchy (System ⊃ Container ⊃ Component).
        (Stakeholder, Requirement),
        (User, Requirement),
        (Requirement, Requirement),
        (Group, Group),
        (Group, Entity),
        (Group, Value),
        (Group, Service),
        (System, Container),
        (Container, Component),
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
        Module,
        Function,
        Struct,
        File,
        Requirement,
        Plan,
        PlanPhase,
        Task,
        Stakeholder,
        Entity,
        System,
        Container,
        Component,
        User,
        Group,
        Value,
        Service,
        Person,
        Constraint,
    ] {
        v.push(("Details", Note, to));
    }
    for to in [
        Module,
        Function,
        Struct,
        File,
        Requirement,
        Plan,
        PlanPhase,
        Task,
        Stakeholder,
        Entity,
        System,
        Container,
        Component,
        User,
        Group,
        Value,
        Service,
        Person,
        Constraint,
    ] {
        v.push(("Reviews", Feedback, to));
    }
    v.push(("Gates", PlanPhase, PlanPhase));
    v.push(("DependsOn", Requirement, Requirement));
    v.push(("Satisfies", PlanPhase, Requirement));
    // Spine edges (§3.3). `drives` runs Requirement → Group/Entity/Value/
    // Service; `represents` runs User → Entity and Entity → Person.
    for to in [Group, Entity, Value, Service] {
        v.push(("Drives", Requirement, to));
    }
    v.push(("Represents", User, Entity));
    v.push(("Represents", Entity, Person));
    for from in [Group, Entity, Service] {
        for to in [System, Container, Component] {
            v.push(("RealisedBy", from, to));
        }
    }
    for from in [System, Container, Component] {
        v.push(("SpecImplementedBy", from, Module));
        v.push(("SpecImplementedBy", from, File));
        v.push(("SpecImplementedBy", from, Struct));
        v.push(("SpecImplementedBy", from, Function));
    }
    v.push(("Publishes", Service, Entity));
    v.push(("Subscribes", Service, Entity));
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
        NodeKind::Requirement => "requirement",
        NodeKind::Note => "note",
        NodeKind::Feedback => "feedback",
        NodeKind::Plan => "plan",
        NodeKind::PlanPhase => "plan_phase",
        NodeKind::Task => "task",
        NodeKind::Scan => "scan",
        NodeKind::Stakeholder => "stakeholder",
        NodeKind::Entity => "entity",
        NodeKind::System => "system",
        NodeKind::Container => "container",
        NodeKind::Component => "component",
        NodeKind::User => "user",
        NodeKind::Group => "group",
        NodeKind::Value => "value",
        NodeKind::Service => "service",
        NodeKind::Person => "person",
        NodeKind::Constraint => "constraint",
    }
}

/// Every `(rel_table, from_label, to_label)` triple the DB schema declares —
/// derived from the same pair enumerations that write the load files
/// (`build_load_files`) and the `CREATE REL TABLE` statements
/// (`create_schema`), so the write-through merge guard in `artifacts.rs`
/// (R3/R4: skip an undeclared pair instead of feeding LadybugDB a Cypher MERGE
/// that throws a binder exception) cannot drift from the schema.
pub fn rel_table_pairs() -> &'static [(&'static str, &'static str, &'static str)] {
    static PAIRS: std::sync::OnceLock<Vec<(&'static str, &'static str, &'static str)>> =
        std::sync::OnceLock::new();
    PAIRS.get_or_init(|| {
        let mut v = Vec::new();
        for (from, to) in contains_pairs() {
            v.push(("Contains", label_of(from), label_of(to)));
        }
        for (table, from, to) in spec_rel_pairs() {
            v.push((table, label_of(from), label_of(to)));
        }
        v
    })
}

/// Every node-table label, in schema creation order. Used for DB label lookups
/// (`ArtifactDb::node_label`) — the label of a `--on` target decides whether
/// it is an allowable `Details` target.
pub fn node_labels() -> &'static [&'static str] {
    &[
        "Module",
        "Scan",
        "Struct",
        "Function",
        "File",
        "UnresolvedTarget",
        "Requirement",
        "Note",
        "Feedback",
        "Plan",
        "PlanPhase",
        "Task",
        "Stakeholder",
        "Entity",
        "System",
        "Container",
        "Component",
        "User",
        "DomainGroup",
        "Value",
        "Service",
        "Person",
        "Constraint",
    ]
}

/// Whether a DB node label is a *code* label (Module/Struct/Function/File/
/// UnresolvedTarget). The placeholder namespace is gone (PHASE_02) and so is
/// placeholder node (PHASE_02) — code vs spec-family discrimination now uses
/// the node label: `review_add` and other FQN routers check `is_code_label`
/// before deriving a project from the FQN.
pub fn is_code_label(label: &str) -> bool {
    matches!(
        label,
        "Module" | "Struct" | "Function" | "File" | "UnresolvedTarget"
    )
}

/// The exact node-table label used in `CREATE REL TABLE` and COPY overrides.
pub fn label_of(k: NodeKind) -> &'static str {
    match k {
        NodeKind::Module => "Module",
        NodeKind::Struct => "Struct",
        NodeKind::Function => "Function",
        NodeKind::File => "File",
        NodeKind::UnresolvedTarget => "UnresolvedTarget",
        NodeKind::Requirement => "Requirement",
        NodeKind::Note => "Note",
        NodeKind::Feedback => "Feedback",
        NodeKind::Plan => "Plan",
        NodeKind::PlanPhase => "PlanPhase",
        NodeKind::Task => "Task",
        NodeKind::Scan => "Scan",
        NodeKind::Stakeholder => "Stakeholder",
        NodeKind::Entity => "Entity",
        NodeKind::System => "System",
        NodeKind::Container => "Container",
        NodeKind::Component => "Component",
        NodeKind::User => "User",
        // `Group` is a reserved LadybugDB keyword (like `GROUP BY`), so the
        // node TABLE label is `DomainGroup` — the FQN type string stays
        // `group` (e.g. `domain.group.<name>`), only the DB table name differs.
        NodeKind::Group => "DomainGroup",
        NodeKind::Value => "Value",
        NodeKind::Service => "Service",
        NodeKind::Person => "Person",
        NodeKind::Constraint => "Constraint",
    }
}

/// Creates the LadybugDB schema (SPEC §7, R1/R2/R20/R21): the five code node
/// tables, the thirteen spec/plan node tables, and the rel tables (Contains
/// extended with spec/plan pairs, plus the nine spec/plan rel tables).
pub fn create_schema(conn: &Connection) -> anyhow::Result<()> {
    conn.query("CREATE NODE TABLE Module(fqn STRING PRIMARY KEY, status STRING)")?;
    conn.query(
        "CREATE NODE TABLE Scan(fqn STRING PRIMARY KEY, git_sha STRING, git_clean STRING, scanned_at STRING)",
    )?;
    conn.query(
        "CREATE NODE TABLE Struct(fqn STRING PRIMARY KEY, path STRING, start INT64, `end` INT64, start_line INT64, end_line INT64, code_type STRING, status STRING)",
    )?;
    conn.query(
        "CREATE NODE TABLE Function(fqn STRING PRIMARY KEY, path STRING, start INT64, `end` INT64, start_line INT64, end_line INT64, code_type STRING, status STRING)",
    )?;
    conn.query(
        "CREATE NODE TABLE File(fqn STRING PRIMARY KEY, start_line INT64, end_line INT64, code_type STRING, status STRING)",
    )?;
    conn.query("CREATE NODE TABLE UnresolvedTarget(fqn STRING PRIMARY KEY, category STRING)")?;
    conn.query(
        "CREATE NODE TABLE Requirement(fqn STRING PRIMARY KEY, id STRING, title STRING, body STRING, feature STRING)",
    )?;
    conn.query("CREATE NODE TABLE Note(fqn STRING PRIMARY KEY, body STRING, kind STRING)")?;
    conn.query(
        "CREATE NODE TABLE Feedback(fqn STRING PRIMARY KEY, body STRING, status STRING, disposition STRING)",
    )?;
    conn.query("CREATE NODE TABLE Plan(fqn STRING PRIMARY KEY, title STRING, strategy STRING)")?;
    conn.query(
        "CREATE NODE TABLE PlanPhase(fqn STRING PRIMARY KEY, number INT64, title STRING, deliverable STRING, status STRING)",
    )?;
    conn.query(
        "CREATE NODE TABLE Task(fqn STRING PRIMARY KEY, title STRING, kind STRING, tier STRING, status STRING)",
    )?;
    conn.query("CREATE NODE TABLE Stakeholder(fqn STRING PRIMARY KEY, name STRING, body STRING)")?;
    conn.query("CREATE NODE TABLE Entity(fqn STRING PRIMARY KEY, name STRING, body STRING)")?;
    conn.query("CREATE NODE TABLE System(fqn STRING PRIMARY KEY, name STRING, body STRING)")?;
    conn.query(
        "CREATE NODE TABLE Container(fqn STRING PRIMARY KEY, name STRING, kind STRING, body STRING)",
    )?;
    conn.query("CREATE NODE TABLE Component(fqn STRING PRIMARY KEY, name STRING, body STRING)")?;
    conn.query("CREATE NODE TABLE User(fqn STRING PRIMARY KEY, name STRING, body STRING)")?;
    conn.query(
        "CREATE NODE TABLE DomainGroup(fqn STRING PRIMARY KEY, name STRING, attribute STRING, root STRING, body STRING)",
    )?;
    conn.query("CREATE NODE TABLE Value(fqn STRING PRIMARY KEY, name STRING, body STRING)")?;
    conn.query("CREATE NODE TABLE Service(fqn STRING PRIMARY KEY, name STRING, body STRING)")?;
    conn.query("CREATE NODE TABLE Person(fqn STRING PRIMARY KEY, name STRING, body STRING)")?;
    conn.query(
        "CREATE NODE TABLE Constraint(fqn STRING PRIMARY KEY, name STRING, body STRING, attaches_to STRING)",
    )?;
    conn.query(
        "CREATE REL TABLE Contains(FROM Module TO Module, FROM Module TO File, FROM File TO Struct, FROM File TO Function, FROM Struct TO Struct, FROM Struct TO Function, FROM Plan TO PlanPhase, FROM PlanPhase TO Task, FROM Stakeholder TO Requirement, FROM User TO Requirement, FROM Requirement TO Requirement, FROM DomainGroup TO DomainGroup, FROM DomainGroup TO Entity, FROM DomainGroup TO Value, FROM DomainGroup TO Service, FROM System TO Container, FROM Container TO Component)",
    )?;
    conn.query("CREATE REL TABLE Calls(FROM Function TO Function, FROM Service TO Service)")?;
    conn.query(
        "CREATE REL TABLE Uses(FROM Function TO Struct, FROM Struct TO Struct, FROM Person TO System)",
    )?;
    conn.query(
        "CREATE REL TABLE UnresolvedCall(FROM Function TO UnresolvedTarget, target_type STRING)",
    )?;
    conn.query(
        "CREATE REL TABLE UnresolvedUse(FROM Function TO UnresolvedTarget, FROM Struct TO UnresolvedTarget)",
    )?;
    conn.query(
        "CREATE REL TABLE Details(FROM Note TO Module, FROM Note TO Function, FROM Note TO Struct, FROM Note TO File, FROM Note TO Requirement, FROM Note TO Plan, FROM Note TO PlanPhase, FROM Note TO Task, FROM Note TO Stakeholder, FROM Note TO Entity, FROM Note TO System, FROM Note TO Container, FROM Note TO Component, FROM Note TO User, FROM Note TO DomainGroup, FROM Note TO Value, FROM Note TO Service, FROM Note TO Person, FROM Note TO Constraint)",
    )?;
    conn.query(
        "CREATE REL TABLE Reviews(FROM Feedback TO Module, FROM Feedback TO Function, FROM Feedback TO Struct, FROM Feedback TO File, FROM Feedback TO Requirement, FROM Feedback TO Plan, FROM Feedback TO PlanPhase, FROM Feedback TO Task, FROM Feedback TO Stakeholder, FROM Feedback TO Entity, FROM Feedback TO System, FROM Feedback TO Container, FROM Feedback TO Component, FROM Feedback TO User, FROM Feedback TO DomainGroup, FROM Feedback TO Value, FROM Feedback TO Service, FROM Feedback TO Person, FROM Feedback TO Constraint)",
    )?;
    conn.query("CREATE REL TABLE DependsOn(FROM Requirement TO Requirement)")?;
    conn.query("CREATE REL TABLE Gates(FROM PlanPhase TO PlanPhase)")?;
    conn.query("CREATE REL TABLE Satisfies(FROM PlanPhase TO Requirement)")?;
    conn.query(
        "CREATE REL TABLE Drives(FROM Requirement TO DomainGroup, FROM Requirement TO Entity, FROM Requirement TO Value, FROM Requirement TO Service)",
    )?;
    conn.query("CREATE REL TABLE Represents(FROM User TO Entity, FROM Entity TO Person)")?;
    conn.query(
        "CREATE REL TABLE RealisedBy(FROM DomainGroup TO System, FROM DomainGroup TO Container, FROM DomainGroup TO Component, FROM Entity TO System, FROM Entity TO Container, FROM Entity TO Component, FROM Service TO System, FROM Service TO Container, FROM Service TO Component)",
    )?;
    conn.query(
        "CREATE REL TABLE SpecImplementedBy(FROM System TO Module, FROM System TO File, FROM System TO Struct, FROM System TO Function, FROM Container TO Module, FROM Container TO File, FROM Container TO Struct, FROM Container TO Function, FROM Component TO Module, FROM Component TO File, FROM Component TO Struct, FROM Component TO Function)",
    )?;
    conn.query("CREATE REL TABLE Publishes(FROM Service TO Entity)")?;
    conn.query("CREATE REL TABLE Subscribes(FROM Service TO Entity)")?;
    Ok(())
}

/// Loads all PARQUET files in `dir` via `COPY FROM`, per `(from, to)` pair for
/// multi-pair rel tables.
pub fn copy_from(conn: &Connection, dir: &Path) -> anyhow::Result<()> {
    let p = |name: &str| dir.join(name).to_string_lossy().into_owned();
    let stmts = [
        format!(r#"COPY Module FROM "{}""#, p("module.parquet")),
        format!(r#"COPY Scan FROM "{}""#, p("scan.parquet")),
        format!(r#"COPY Struct FROM "{}""#, p("struct.parquet")),
        format!(r#"COPY Function FROM "{}""#, p("function.parquet")),
        format!(r#"COPY File FROM "{}""#, p("file.parquet")),
        format!(
            r#"COPY UnresolvedTarget FROM "{}""#,
            p("unresolved.parquet")
        ),
        format!(r#"COPY Requirement FROM "{}""#, p("requirement.parquet")),
        format!(r#"COPY Note FROM "{}""#, p("note.parquet")),
        format!(r#"COPY Feedback FROM "{}""#, p("feedback.parquet")),
        format!(r#"COPY Plan FROM "{}""#, p("plan.parquet")),
        format!(r#"COPY PlanPhase FROM "{}""#, p("plan_phase.parquet")),
        format!(r#"COPY Task FROM "{}""#, p("task.parquet")),
        format!(r#"COPY Stakeholder FROM "{}""#, p("stakeholder.parquet")),
        format!(r#"COPY Entity FROM "{}""#, p("entity.parquet")),
        format!(r#"COPY System FROM "{}""#, p("system.parquet")),
        format!(r#"COPY Container FROM "{}""#, p("container.parquet")),
        format!(r#"COPY Component FROM "{}""#, p("component.parquet")),
        format!(r#"COPY User FROM "{}""#, p("user.parquet")),
        format!(r#"COPY DomainGroup FROM "{}""#, p("group.parquet")),
        format!(r#"COPY Value FROM "{}""#, p("value.parquet")),
        format!(r#"COPY Service FROM "{}""#, p("service.parquet")),
        format!(r#"COPY Person FROM "{}""#, p("person.parquet")),
        format!(r#"COPY Constraint FROM "{}""#, p("constraint.parquet")),
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
        format!(
            r#"COPY Calls FROM "{}" (from="Function", to="Function")"#,
            p("calls_fn.parquet")
        ),
        format!(
            r#"COPY Calls FROM "{}" (from="Service", to="Service")"#,
            p("calls_svc.parquet")
        ),
        format!(
            r#"COPY Uses FROM "{}" (from="Function", to="Struct")"#,
            p("uses_fn.parquet")
        ),
        format!(
            r#"COPY Uses FROM "{}" (from="Struct", to="Struct")"#,
            p("uses_struct.parquet")
        ),
        format!(
            r#"COPY Uses FROM "{}" (from="Person", to="System")"#,
            p("uses_person.parquet")
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
    /// The export control record, written as **line 1** of graph.jsonl from a
    /// `Scan` graph node (the git state the scan ran under). Git fields are
    /// absent when the scan was not in a git repo.
    ScanMeta {
        #[serde(skip_serializing_if = "Option::is_none")]
        git_sha: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        git_clean: Option<bool>,
        scanned_at: String,
    },
    Module {
        fqn: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        status: String,
    },
    Struct {
        fqn: String,
        path: String,
        start: u32,
        end: u32,
        start_line: u32,
        end_line: u32,
        code_type: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        status: String,
    },
    Function {
        fqn: String,
        path: String,
        start: u32,
        end: u32,
        start_line: u32,
        end_line: u32,
        code_type: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        status: String,
    },
    File {
        fqn: String,
        start_line: u32,
        end_line: u32,
        code_type: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        status: String,
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
        #[serde(skip_serializing_if = "String::is_empty")]
        status: String,
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
    Stakeholder {
        fqn: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        name: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        body: String,
    },
    Entity {
        fqn: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        name: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        body: String,
    },
    System {
        fqn: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        name: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        body: String,
    },
    Container {
        fqn: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        name: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        kind: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        body: String,
    },
    Component {
        fqn: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        name: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        body: String,
    },
    User {
        fqn: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        name: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        body: String,
    },
    Group {
        fqn: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        name: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        attribute: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        root: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        body: String,
    },
    Value {
        fqn: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        name: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        body: String,
    },
    Service {
        fqn: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        name: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        body: String,
    },
    Person {
        fqn: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        name: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        body: String,
    },
    Constraint {
        fqn: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        name: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        body: String,
        #[serde(rename = "attaches-to", skip_serializing_if = "String::is_empty")]
        attaches_to: String,
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
    Satisfies {
        from: String,
        to: String,
    },
    Drives {
        from: String,
        to: String,
    },
    Represents {
        from: String,
        to: String,
    },
    #[serde(rename = "realised-by")]
    RealisedBy {
        from: String,
        to: String,
    },
    #[serde(rename = "implemented-by")]
    SpecImplementedBy {
        from: String,
        to: String,
    },
    Publishes {
        from: String,
        to: String,
    },
    Subscribes {
        from: String,
        to: String,
    },
}

/// Writes `graph.jsonl`: the final graph re-serialized with canonical FQNs
/// (nodes) and resolved endpoints (edges), without opaque ids. A `Scan` node
/// exports as a `scan_meta` control record on **line 1** (before every node
/// and edge line), mirroring the `lang_switch` control-record convention.
pub fn write_graph_jsonl(graph: &Graph, path: &Path) -> anyhow::Result<()> {
    let file = File::create(path)?;
    let mut w = BufWriter::new(file);

    let write_line = |w: &mut BufWriter<File>, v: &Export| -> anyhow::Result<()> {
        w.write_all(serde_json::to_string(v)?.as_bytes())?;
        w.write_all(b"\n")?;
        Ok(())
    };

    // The scan_meta control record leads the export (line 1). A Scan node is
    // not emitted again in the node loop below.
    for (_, node) in graph.nodes.iter().filter(|(_, n)| n.kind == NodeKind::Scan) {
        write_line(
            &mut w,
            &Export::ScanMeta {
                git_sha: node.git_sha.clone(),
                git_clean: node.git_clean,
                scanned_at: node.scanned_at.clone().unwrap_or_default(),
            },
        )?;
    }

    for (fqn, node) in &graph.nodes {
        let rec = match node.kind {
            NodeKind::Scan => continue,
            NodeKind::Module => Export::Module {
                fqn: fqn.clone(),
                status: node.status.clone().unwrap_or_default(),
            },
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
                    status: node.status.clone().unwrap_or_default(),
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
                    status: node.status.clone().unwrap_or_default(),
                }
            }
            NodeKind::File => {
                let (start_line, end_line) = lines(graph, fqn);
                Export::File {
                    fqn: fqn.clone(),
                    start_line: start_line as u32,
                    end_line: end_line as u32,
                    code_type: node.code_type.clone(),
                    status: node.status.clone().unwrap_or_default(),
                }
            }
            NodeKind::UnresolvedTarget => Export::Unresolved {
                fqn: fqn.clone(),
                category: node.category.clone().unwrap_or_default(),
            },
            NodeKind::Requirement => Export::Requirement {
                fqn: fqn.clone(),
                id: node.id.clone().unwrap_or_default(),
                title: node.title.clone().unwrap_or_default(),
                body: node.body.clone().unwrap_or_default(),
                feature: node.feature.clone().unwrap_or_default(),
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
                status: node.status.clone().unwrap_or_default(),
            },
            NodeKind::Task => Export::Task {
                fqn: fqn.clone(),
                title: node.title.clone().unwrap_or_default(),
                kind: node.sub_kind.clone().unwrap_or_default(),
                tier: node.tier.clone().unwrap_or_default(),
                status: node.status.clone().unwrap_or_default(),
            },
            NodeKind::Stakeholder => Export::Stakeholder {
                fqn: fqn.clone(),
                name: node.name.clone().unwrap_or_default(),
                body: node.body.clone().unwrap_or_default(),
            },
            NodeKind::Entity => Export::Entity {
                fqn: fqn.clone(),
                name: node.name.clone().unwrap_or_default(),
                body: node.body.clone().unwrap_or_default(),
            },
            NodeKind::System => Export::System {
                fqn: fqn.clone(),
                name: node.name.clone().unwrap_or_default(),
                body: node.body.clone().unwrap_or_default(),
            },
            NodeKind::Container => Export::Container {
                fqn: fqn.clone(),
                name: node.name.clone().unwrap_or_default(),
                kind: node.sub_kind.clone().unwrap_or_default(),
                body: node.body.clone().unwrap_or_default(),
            },
            NodeKind::Component => Export::Component {
                fqn: fqn.clone(),
                name: node.name.clone().unwrap_or_default(),
                body: node.body.clone().unwrap_or_default(),
            },
            NodeKind::User => Export::User {
                fqn: fqn.clone(),
                name: node.name.clone().unwrap_or_default(),
                body: node.body.clone().unwrap_or_default(),
            },
            NodeKind::Group => Export::Group {
                fqn: fqn.clone(),
                name: node.name.clone().unwrap_or_default(),
                attribute: node.attribute.clone().unwrap_or_default(),
                root: node.root.clone().unwrap_or_default(),
                body: node.body.clone().unwrap_or_default(),
            },
            NodeKind::Value => Export::Value {
                fqn: fqn.clone(),
                name: node.name.clone().unwrap_or_default(),
                body: node.body.clone().unwrap_or_default(),
            },
            NodeKind::Service => Export::Service {
                fqn: fqn.clone(),
                name: node.name.clone().unwrap_or_default(),
                body: node.body.clone().unwrap_or_default(),
            },
            NodeKind::Person => Export::Person {
                fqn: fqn.clone(),
                name: node.name.clone().unwrap_or_default(),
                body: node.body.clone().unwrap_or_default(),
            },
            NodeKind::Constraint => Export::Constraint {
                fqn: fqn.clone(),
                name: node.name.clone().unwrap_or_default(),
                body: node.body.clone().unwrap_or_default(),
                attaches_to: node.attaches_to.clone().unwrap_or_default(),
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
    for (a, b) in &graph.satisfies {
        write_line(
            &mut w,
            &Export::Satisfies {
                from: a.clone(),
                to: b.clone(),
            },
        )?;
    }
    for (a, b) in &graph.drives {
        write_line(
            &mut w,
            &Export::Drives {
                from: a.clone(),
                to: b.clone(),
            },
        )?;
    }
    for (a, b) in &graph.represents {
        write_line(
            &mut w,
            &Export::Represents {
                from: a.clone(),
                to: b.clone(),
            },
        )?;
    }
    for (a, b) in &graph.realised_by {
        write_line(
            &mut w,
            &Export::RealisedBy {
                from: a.clone(),
                to: b.clone(),
            },
        )?;
    }
    for (a, b) in &graph.spec_implemented_by {
        write_line(
            &mut w,
            &Export::SpecImplementedBy {
                from: a.clone(),
                to: b.clone(),
            },
        )?;
    }
    for (a, b) in &graph.publishes {
        write_line(
            &mut w,
            &Export::Publishes {
                from: a.clone(),
                to: b.clone(),
            },
        )?;
    }
    for (a, b) in &graph.subscribes {
        write_line(
            &mut w,
            &Export::Subscribes {
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

    /// Reads `graph.jsonl` back into a [`Graph`] — the re-ingest leg of the
    /// export round-trip (PHASE_01 done gate: "JSONL → DB → JSONL round-trip for
    /// each" node/edge kind). Mirror of [`write_graph_jsonl`]: every `Export`
    /// record line is mapped back to the graph node or edge it came from. A
    /// `scan_meta` control record on line 1 reconstructs the `Scan` node at
    /// `SCAN_HEAD` (it is never emitted as a node line). Unknown `type`s are an
    /// error so a new export kind cannot silently vanish on the way back in.
    pub fn read_graph_jsonl(path: &Path) -> anyhow::Result<Graph> {
        let text = std::fs::read_to_string(path)?;
        let mut g = Graph::default();

        for line in text.lines() {
            let v: serde_json::Value = serde_json::from_str(line)?;
            let t = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
            let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
            let o = |k: &str| v.get(k).and_then(|x| x.as_str()).map(str::to_string);
            let u = |k: &str| v.get(k).and_then(|x| x.as_u64()).unwrap_or(0) as u32;
            let located = || {
                Some(Location {
                    path: s("path").into(),
                    start: u("start"),
                    end: u("end"),
                    start_line: u("start_line"),
                    end_line: u("end_line"),
                })
            };
            match t {
                "scan_meta" => {
                    let mut n = Node {
                        kind: NodeKind::Scan,
                        ..Node::default()
                    };
                    n.git_sha = o("git_sha");
                    n.git_clean = v.get("git_clean").and_then(|x| x.as_bool());
                    n.scanned_at = o("scanned_at");
                    g.nodes.insert(crate::schema::SCAN_HEAD.to_string(), n);
                }
                "module" => {
                    g.nodes.insert(
                        s("fqn"),
                        Node {
                            kind: NodeKind::Module,
                            status: o("status"),
                            ..Node::default()
                        },
                    );
                }
                "struct" => {
                    let fqn = s("fqn");
                    g.nodes.insert(
                        fqn,
                        Node {
                            kind: NodeKind::Struct,
                            location: located(),
                            code_type: s("code_type"),
                            status: o("status"),
                            ..Node::default()
                        },
                    );
                }
                "function" => {
                    let fqn = s("fqn");
                    g.nodes.insert(
                        fqn,
                        Node {
                            kind: NodeKind::Function,
                            location: located(),
                            code_type: s("code_type"),
                            status: o("status"),
                            ..Node::default()
                        },
                    );
                }
                "file" => {
                    // A File node's fqn IS its absolute path (no separate path
                    // field is exported); the line range rides as the span.
                    let fqn = s("fqn");
                    g.nodes.insert(
                        fqn.clone(),
                        Node {
                            kind: NodeKind::File,
                            location: Some(Location {
                                path: fqn.into(),
                                start: 0,
                                end: 0,
                                start_line: u("start_line"),
                                end_line: u("end_line"),
                            }),
                            code_type: s("code_type"),
                            status: o("status"),
                            ..Node::default()
                        },
                    );
                }
                "unresolved" => {
                    g.nodes.insert(
                        s("fqn"),
                        Node {
                            kind: NodeKind::UnresolvedTarget,
                            category: o("category"),
                            ..Node::default()
                        },
                    );
                }
                "requirement" => {
                    g.nodes.insert(
                        s("fqn"),
                        Node {
                            kind: NodeKind::Requirement,
                            id: o("id"),
                            title: o("title"),
                            body: o("body"),
                            feature: o("feature"),
                            ..Node::default()
                        },
                    );
                }
                "note" => {
                    g.nodes.insert(
                        s("fqn"),
                        Node {
                            kind: NodeKind::Note,
                            body: o("body"),
                            sub_kind: o("kind"),
                            ..Node::default()
                        },
                    );
                }
                "feedback" => {
                    g.nodes.insert(
                        s("fqn"),
                        Node {
                            kind: NodeKind::Feedback,
                            body: o("body"),
                            status: o("status"),
                            disposition: o("disposition"),
                            ..Node::default()
                        },
                    );
                }
                "plan" => {
                    g.nodes.insert(
                        s("fqn"),
                        Node {
                            kind: NodeKind::Plan,
                            title: o("title"),
                            strategy: o("strategy"),
                            ..Node::default()
                        },
                    );
                }
                "plan_phase" => {
                    g.nodes.insert(
                        s("fqn"),
                        Node {
                            kind: NodeKind::PlanPhase,
                            number: v.get("number").and_then(|x| x.as_u64()).map(|x| x as u32),
                            title: o("title"),
                            deliverable: o("deliverable"),
                            status: o("status"),
                            ..Node::default()
                        },
                    );
                }
                "task" => {
                    g.nodes.insert(
                        s("fqn"),
                        Node {
                            kind: NodeKind::Task,
                            title: o("title"),
                            sub_kind: o("kind"),
                            tier: o("tier"),
                            status: o("status"),
                            ..Node::default()
                        },
                    );
                }
                "stakeholder" => {
                    g.nodes.insert(
                        s("fqn"),
                        Node {
                            kind: NodeKind::Stakeholder,
                            name: o("name"),
                            body: o("body"),
                            ..Node::default()
                        },
                    );
                }
                "entity" => {
                    g.nodes.insert(
                        s("fqn"),
                        Node {
                            kind: NodeKind::Entity,
                            name: o("name"),
                            body: o("body"),
                            ..Node::default()
                        },
                    );
                }
                "system" => {
                    g.nodes.insert(
                        s("fqn"),
                        Node {
                            kind: NodeKind::System,
                            name: o("name"),
                            body: o("body"),
                            ..Node::default()
                        },
                    );
                }
                "container" => {
                    g.nodes.insert(
                        s("fqn"),
                        Node {
                            kind: NodeKind::Container,
                            name: o("name"),
                            sub_kind: o("kind"),
                            body: o("body"),
                            ..Node::default()
                        },
                    );
                }
                "component" => {
                    g.nodes.insert(
                        s("fqn"),
                        Node {
                            kind: NodeKind::Component,
                            name: o("name"),
                            body: o("body"),
                            ..Node::default()
                        },
                    );
                }
                "user" => {
                    g.nodes.insert(
                        s("fqn"),
                        Node {
                            kind: NodeKind::User,
                            name: o("name"),
                            body: o("body"),
                            ..Node::default()
                        },
                    );
                }
                "group" => {
                    g.nodes.insert(
                        s("fqn"),
                        Node {
                            kind: NodeKind::Group,
                            name: o("name"),
                            attribute: o("attribute"),
                            root: o("root"),
                            body: o("body"),
                            ..Node::default()
                        },
                    );
                }
                "value" => {
                    g.nodes.insert(
                        s("fqn"),
                        Node {
                            kind: NodeKind::Value,
                            name: o("name"),
                            body: o("body"),
                            ..Node::default()
                        },
                    );
                }
                "service" => {
                    g.nodes.insert(
                        s("fqn"),
                        Node {
                            kind: NodeKind::Service,
                            name: o("name"),
                            body: o("body"),
                            ..Node::default()
                        },
                    );
                }
                "person" => {
                    g.nodes.insert(
                        s("fqn"),
                        Node {
                            kind: NodeKind::Person,
                            name: o("name"),
                            body: o("body"),
                            ..Node::default()
                        },
                    );
                }
                "constraint" => {
                    g.nodes.insert(
                        s("fqn"),
                        Node {
                            kind: NodeKind::Constraint,
                            name: o("name"),
                            body: o("body"),
                            attaches_to: o("attaches-to"),
                            ..Node::default()
                        },
                    );
                }
                "contains" => {
                    g.contains.insert((s("from"), s("to")));
                }
                "calls" => {
                    g.calls.insert((s("from"), s("to")));
                }
                "uses" => {
                    g.uses.insert((s("from"), s("to")));
                }
                "unresolved_call" => {
                    g.unresolved_calls
                        .insert((s("from"), s("to"), s("target_type")));
                }
                "unresolved_use" => {
                    g.unresolved_uses.insert((s("from"), s("to")));
                }
                "details" => {
                    g.details.insert((s("from"), s("to")));
                }
                "reviews" => {
                    g.reviews.insert((s("from"), s("to")));
                }
                "depends_on" => {
                    g.depends_on.insert((s("from"), s("to")));
                }
                "gates" => {
                    g.gates.insert((s("from"), s("to")));
                }
                "satisfies" => {
                    g.satisfies.insert((s("from"), s("to")));
                }
                "drives" => {
                    g.drives.insert((s("from"), s("to")));
                }
                "represents" => {
                    g.represents.insert((s("from"), s("to")));
                }
                "realised-by" => {
                    g.realised_by.insert((s("from"), s("to")));
                }
                "implemented-by" => {
                    g.spec_implemented_by.insert((s("from"), s("to")));
                }
                "publishes" => {
                    g.publishes.insert((s("from"), s("to")));
                }
                "subscribes" => {
                    g.subscribes.insert((s("from"), s("to")));
                }
                other => {
                    anyhow::bail!("graph.jsonl record with unknown type `{other}`");
                }
            }
        }
        Ok(g)
    }
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
        // A plan + requirement graph survives the PARQUET load path: node
        // tables with their own columns, multi-pair Contains, and the
        // spec/plan rel tables (one COPY per pair, empty pairs too).
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
            "foo/plan",
            Node {
                title: Some("Plan".to_string()),
                strategy: Some("Layer-first".to_string()),
                ..sp(NodeKind::Plan)
            },
        );
        n(
            "foo/plan.phase-1",
            Node {
                number: Some(1),
                title: Some("P1".to_string()),
                deliverable: Some("Core".to_string()),
                ..sp(NodeKind::PlanPhase)
            },
        );
        n(
            "foo/plan.phase-1.task-1",
            Node {
                title: Some("Add RootStore".to_string()),
                sub_kind: Some("source".to_string()),
                status: Some("pending".to_string()),
                ..sp(NodeKind::Task)
            },
        );
        n(
            "foo/spec.R1",
            Node {
                id: Some("R1".to_string()),
                title: Some("Timer".to_string()),
                feature: Some("feature-a".to_string()),
                ..sp(NodeKind::Requirement)
            },
        );
        n(
            "foo/gateway",
            Node {
                name: Some("Gateway".to_string()),
                status: Some("planned".to_string()),
                ..sp(NodeKind::Struct)
            },
        );
        n(
            "foo/note-1",
            Node {
                body: Some("Background prose".to_string()),
                sub_kind: Some("background".to_string()),
                ..sp(NodeKind::Note)
            },
        );
        n(
            "foo/feedback-1",
            Node {
                body: Some("Split R1".to_string()),
                status: Some("open".to_string()),
                ..sp(NodeKind::Feedback)
            },
        );

        g.contains.extend([
            ("foo/plan".into(), "foo/plan.phase-1".into()),
            ("foo/plan.phase-1".into(), "foo/plan.phase-1.task-1".into()),
        ]);
        g.details.insert(("foo/note-1".into(), "foo/plan".into()));
        g.reviews
            .insert(("foo/feedback-1".into(), "foo/spec.R1".into()));
        g.depends_on
            .insert(("foo/spec.R1".into(), "foo/spec.R1".into()));
        g.satisfies
            .insert(("foo/plan.phase-1".into(), "foo/spec.R1".into()));

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
            .query("MATCH (p:Plan) RETURN p.fqn, p.title, p.strategy")
            .unwrap()
            .to_string();
        assert!(
            out.contains("foo/plan") && out.contains("Layer-first"),
            "plan rows: {out}"
        );
        let out = conn
            .query("MATCH (r:Requirement) RETURN r.id, r.feature")
            .unwrap()
            .to_string();
        assert!(
            out.contains("R1") && out.contains("feature-a"),
            "req rows: {out}"
        );
        let out = conn
            .query("MATCH (p:PlanPhase) RETURN p.number, p.deliverable")
            .unwrap()
            .to_string();
        assert!(
            out.contains("1") && out.contains("Core"),
            "plan phase rows: {out}"
        );
        let out = conn
            .query("MATCH (t:Task) RETURN t.kind, t.status")
            .unwrap()
            .to_string();
        assert!(
            out.contains("source") && out.contains("pending"),
            "task rows: {out}"
        );
        let out = conn
            .query("MATCH (s:Struct {fqn: 'foo/gateway'}) RETURN s.status")
            .unwrap()
            .to_string();
        assert!(out.contains("planned"), "planned struct row: {out}");

        // Multi-pair Contains: Plan -> PlanPhase -> Task.
        let out = conn
            .query("MATCH (p:Plan)-[:Contains]->(pp:PlanPhase) RETURN pp.fqn")
            .unwrap()
            .to_string();
        assert!(
            out.contains("foo/plan.phase-1"),
            "contains plan->phase: {out}"
        );

        // Spec/plan rel tables.
        let out = conn
            .query("MATCH (:Note)-[:Details]->(p:Plan) RETURN p.fqn")
            .unwrap()
            .to_string();
        assert!(out.contains("foo/plan"), "details: {out}");
        let out = conn
            .query("MATCH (:Feedback)-[:Reviews]->(r:Requirement) RETURN r.fqn")
            .unwrap()
            .to_string();
        assert!(out.contains("foo/spec.R1"), "reviews: {out}");
        let out = conn
            .query("MATCH (a:Requirement)-[:DependsOn]->(b:Requirement) RETURN b.fqn")
            .unwrap()
            .to_string();
        assert!(out.contains("foo/spec.R1"), "depends_on: {out}");
        let out = conn
            .query("MATCH (:PlanPhase)-[:Satisfies]->(r:Requirement) RETURN r.fqn")
            .unwrap()
            .to_string();
        assert!(out.contains("foo/spec.R1"), "satisfies: {out}");

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

    /// A fixture graph with one `Scan` node added (a scan that ran in a clean
    /// git repo at `abc123`).
    fn fixture_graph_with_scan() -> Graph {
        let mut g = fixture_graph();
        g.nodes.insert(
            crate::schema::SCAN_HEAD.to_string(),
            Node {
                kind: NodeKind::Scan,
                git_sha: Some("abc123".to_string()),
                git_clean: Some(true),
                scanned_at: Some("2026-09-07T00:00:00Z".to_string()),
                ..Node::default()
            },
        );
        g
    }

    #[test]
    fn scan_meta_is_graph_jsonl_line_one() {
        let dir = std::env::temp_dir().join(format!("apg-test-scanmeta-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let graph = fixture_graph_with_scan();
        let out_path = dir.join("graph.jsonl");
        write_graph_jsonl(&graph, &out_path).unwrap();

        let text = std::fs::read_to_string(&out_path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        // Line 1 is the scan_meta control record with the git fields.
        let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(first["type"], "scan_meta");
        assert_eq!(first["git_sha"], "abc123");
        assert_eq!(first["git_clean"], true);
        assert_eq!(first["scanned_at"], "2026-09-07T00:00:00Z");
        assert!(
            first.get("fqn").is_none(),
            "scan_meta is a control record, not a node"
        );
        // The Scan node is exported exactly once (line 1), not again as a node.
        assert_eq!(
            lines.len(),
            graph.nodes.len()
                + graph.contains.len()
                + graph.calls.len()
                + graph.uses.len()
                + graph.unresolved_calls.len()
                + graph.unresolved_uses.len()
        );
        // No later line is a scan node record.
        for line in &lines[1..] {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            assert_ne!(v["type"], "scan", "scan node leaked into export: {line}");
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn graph_jsonl_roundtrips_tier_nodes_and_spine() {
        // The export is round-trippable: write_graph_jsonl → read_graph_jsonl →
        // fresh DB, and every §3.1 catalog node + §3.3 spine edge survives. A
        // real `Scan` node round-trips too (scan_meta control record on line 1).
        let dir =
            std::env::temp_dir().join(format!("apg-test-spine-roundtrip-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let mut g = fixture_graph_with_scan();
        let sp = |kind: NodeKind| Node {
            kind,
            code_type: String::new(),
            ..Node::default()
        };
        let mut n = |fqn: &str, node: Node| {
            g.nodes.insert(fqn.to_string(), node);
        };
        // The §3.1 catalog (requirements + domain + solution).
        n(
            "requirements.requirement.auth",
            Node {
                id: Some("R1".to_string()),
                title: Some("Users authenticate".to_string()),
                ..sp(NodeKind::Requirement)
            },
        );
        n(
            "requirements.stakeholder.ops",
            Node {
                name: Some("ops".to_string()),
                ..sp(NodeKind::Stakeholder)
            },
        );
        n(
            "requirements.user.customer",
            Node {
                name: Some("customer".to_string()),
                ..sp(NodeKind::User)
            },
        );
        n(
            "domain.group.sales",
            Node {
                name: Some("sales".to_string()),
                attribute: Some("core".to_string()),
                ..sp(NodeKind::Group)
            },
        );
        n(
            "domain.entity.user",
            Node {
                name: Some("user".to_string()),
                ..sp(NodeKind::Entity)
            },
        );
        n(
            "domain.value.money",
            Node {
                name: Some("money".to_string()),
                ..sp(NodeKind::Value)
            },
        );
        n(
            "domain.service.checkout",
            Node {
                name: Some("checkout".to_string()),
                ..sp(NodeKind::Service)
            },
        );
        n(
            "solution.system.platform",
            Node {
                name: Some("platform".to_string()),
                ..sp(NodeKind::System)
            },
        );
        n(
            "solution.container.api",
            Node {
                name: Some("api".to_string()),
                sub_kind: Some("app".to_string()),
                ..sp(NodeKind::Container)
            },
        );
        n(
            "solution.component.gateway",
            Node {
                name: Some("gateway".to_string()),
                ..sp(NodeKind::Component)
            },
        );
        n(
            "solution.person.alice",
            Node {
                name: Some("alice".to_string()),
                ..sp(NodeKind::Person)
            },
        );
        // Tier 4: the code the component is implemented by.
        n(
            "mod.Gateway",
            Node {
                kind: NodeKind::Struct,
                code_type: "src".to_string(),
                ..Node::default()
            },
        );
        // The §3.3 hierarchy + spine.
        g.contains.extend([
            (
                "requirements.stakeholder.ops".into(),
                "requirements.requirement.auth".into(),
            ),
            (
                "requirements.user.customer".into(),
                "requirements.requirement.auth".into(),
            ),
            ("domain.group.sales".into(), "domain.entity.user".into()),
            ("domain.group.sales".into(), "domain.value.money".into()),
            (
                "domain.group.sales".into(),
                "domain.service.checkout".into(),
            ),
            (
                "solution.system.platform".into(),
                "solution.container.api".into(),
            ),
            (
                "solution.container.api".into(),
                "solution.component.gateway".into(),
            ),
        ]);
        g.drives.insert((
            "requirements.requirement.auth".to_string(),
            "domain.group.sales".to_string(),
        ));
        g.represents.insert((
            "requirements.user.customer".to_string(),
            "domain.entity.user".to_string(),
        ));
        g.represents.insert((
            "domain.entity.user".to_string(),
            "solution.person.alice".to_string(),
        ));
        g.realised_by.insert((
            "domain.group.sales".to_string(),
            "solution.system.platform".to_string(),
        ));
        g.spec_implemented_by.insert((
            "solution.component.gateway".to_string(),
            "mod.Gateway".to_string(),
        ));

        let out_path = dir.join("graph.jsonl");
        write_graph_jsonl(&g, &out_path).unwrap();

        // Re-ingest the export: read_graph_jsonl rebuilds the graph (scan_meta
        // line 1 → the Scan node, catalog nodes, Contains + every spine edge).
        let back = read_graph_jsonl(&out_path).unwrap();

        // Every node survives with its kind.
        for (fqn, node) in &g.nodes {
            let seen = back
                .nodes
                .get(fqn)
                .unwrap_or_else(|| panic!("{fqn} lost in round-trip"));
            assert_eq!(seen.kind, node.kind, "{fqn} kind");
        }
        assert_eq!(back.nodes.len(), g.nodes.len(), "node count");
        // Edge sets are identical — nothing projected away.
        assert_eq!(back.contains, g.contains, "contains edges");
        for (name, a, b) in [
            ("drives", &back.drives, &g.drives),
            ("represents", &back.represents, &g.represents),
            ("realised_by", &back.realised_by, &g.realised_by),
            (
                "spec_implemented_by",
                &back.spec_implemented_by,
                &g.spec_implemented_by,
            ),
            ("calls", &back.calls, &g.calls),
            ("uses", &back.uses, &g.uses),
            ("unresolved_uses", &back.unresolved_uses, &g.unresolved_uses),
        ] {
            assert_eq!(a, b, "{name} edges");
        }
        assert_eq!(
            back.unresolved_calls, g.unresolved_calls,
            "unresolved_calls edges"
        );

        // The closed loop lands in a queryable DB: load the re-ingested graph
        // into a fresh DB and resolve every catalog label + the spine end to end.
        let ldir = dir.join("load");
        std::fs::create_dir_all(&ldir).unwrap();
        build_load_files(&back, &ldir).unwrap();
        let db = Database::in_memory(SystemConfig::default()).unwrap();
        let conn = Connection::new(&db).unwrap();
        create_schema(&conn).unwrap();
        copy_from(&conn, &ldir).unwrap();

        for (label, fqn) in [
            ("Stakeholder", "requirements.stakeholder.ops"),
            ("User", "requirements.user.customer"),
            ("Requirement", "requirements.requirement.auth"),
            ("DomainGroup", "domain.group.sales"),
            ("Entity", "domain.entity.user"),
            ("Value", "domain.value.money"),
            ("Service", "domain.service.checkout"),
            ("System", "solution.system.platform"),
            ("Container", "solution.container.api"),
            ("Component", "solution.component.gateway"),
            ("Person", "solution.person.alice"),
        ] {
            let out = conn
                .query(&format!("MATCH (n:{label} {{fqn: '{fqn}'}}) RETURN n.fqn"))
                .unwrap()
                .to_string();
            assert!(
                !out.contains("(empty)"),
                "{label} {fqn} missing after round-trip: {out}"
            );
        }
        let out = conn
            .query(
                "MATCH (:Requirement)-[:Drives]->(g:DomainGroup)-[:RealisedBy]->(:System)-[:Contains]->(:Container)-[:Contains]->(c:Component)-[:SpecImplementedBy]->(impl) RETURN impl.fqn",
            )
            .unwrap()
            .to_string();
        assert!(
            out.contains("mod.Gateway"),
            "spine to code after round-trip: {out}"
        );
        let out = conn
            .query("MATCH (s:Scan) RETURN s.git_sha, s.git_clean")
            .unwrap()
            .to_string();
        assert!(
            out.contains("abc123") && out.contains("true"),
            "scan survived round-trip: {out}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_node_loads_into_db() {
        let dir = std::env::temp_dir().join(format!("apg-test-scandb-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let graph = fixture_graph_with_scan();
        build_load_files(&graph, &dir).unwrap();

        let db = Database::in_memory(SystemConfig::default()).unwrap();
        let conn = Connection::new(&db).unwrap();
        create_schema(&conn).unwrap();
        copy_from(&conn, &dir).unwrap();

        let out = conn
            .query("MATCH (s:Scan) RETURN s.fqn, s.git_sha, s.git_clean, s.scanned_at")
            .unwrap()
            .to_string();
        assert!(
            out.contains("scan/HEAD") && out.contains("abc123") && out.contains("true"),
            "scan rows: {out}"
        );
        assert!(
            out.contains("2026-09-07T00:00:00Z"),
            "scan scanned_at: {out}"
        );

        // A graph without a Scan node still loads (empty Scan table).
        let dir2 = std::env::temp_dir().join(format!("apg-test-scandb2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir2);
        std::fs::create_dir_all(&dir2).unwrap();
        build_load_files(&fixture_graph(), &dir2).unwrap();
        let db2 = Database::in_memory(SystemConfig::default()).unwrap();
        let conn2 = Connection::new(&db2).unwrap();
        create_schema(&conn2).unwrap();
        copy_from(&conn2, &dir2).unwrap();
        let out2 = conn2
            .query("MATCH (s:Scan) RETURN count(*)")
            .unwrap()
            .to_string();
        assert!(out2.contains("0"), "expected empty Scan table, got: {out2}");

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&dir2);
    }
}
