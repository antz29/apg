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
    let mut domain_fqn = Vec::new();
    let mut domain_name = Vec::new();
    let mut domain_body = Vec::new();
    let mut subdomain_fqn = Vec::new();
    let mut subdomain_name = Vec::new();
    let mut subdomain_kind = Vec::new();
    let mut subdomain_body = Vec::new();
    let mut entity_fqn = Vec::new();
    let mut entity_name = Vec::new();
    let mut entity_body = Vec::new();
    let mut value_object_fqn = Vec::new();
    let mut value_object_name = Vec::new();
    let mut value_object_body = Vec::new();
    let mut aggregate_fqn = Vec::new();
    let mut aggregate_name = Vec::new();
    let mut aggregate_root = Vec::new();
    let mut aggregate_body = Vec::new();
    let mut domain_event_fqn = Vec::new();
    let mut domain_event_name = Vec::new();
    let mut domain_event_body = Vec::new();
    let mut domain_process_fqn = Vec::new();
    let mut domain_process_name = Vec::new();
    let mut domain_process_body = Vec::new();
    let mut domain_rule_fqn = Vec::new();
    let mut domain_rule_name = Vec::new();
    let mut domain_rule_body = Vec::new();
    let mut actor_fqn = Vec::new();
    let mut actor_name = Vec::new();
    let mut actor_body = Vec::new();
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
    let mut invariant_fqn = Vec::new();
    let mut invariant_title = Vec::new();
    let mut invariant_body = Vec::new();
    let mut invariant_category = Vec::new();
    let mut invariant_scope = Vec::new();
    let mut invariant_status = Vec::new();

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
            NodeKind::Domain => {
                domain_fqn.push(fqn.clone());
                domain_name.push(node.name.clone().unwrap_or_default());
                domain_body.push(node.body.clone().unwrap_or_default());
            }
            NodeKind::Subdomain => {
                subdomain_fqn.push(fqn.clone());
                subdomain_name.push(node.name.clone().unwrap_or_default());
                subdomain_kind.push(node.sub_kind.clone().unwrap_or_default());
                subdomain_body.push(node.body.clone().unwrap_or_default());
            }
            NodeKind::Entity => {
                entity_fqn.push(fqn.clone());
                entity_name.push(node.name.clone().unwrap_or_default());
                entity_body.push(node.body.clone().unwrap_or_default());
            }
            NodeKind::ValueObject => {
                value_object_fqn.push(fqn.clone());
                value_object_name.push(node.name.clone().unwrap_or_default());
                value_object_body.push(node.body.clone().unwrap_or_default());
            }
            NodeKind::Aggregate => {
                aggregate_fqn.push(fqn.clone());
                aggregate_name.push(node.name.clone().unwrap_or_default());
                aggregate_root.push(node.root.clone().unwrap_or_default());
                aggregate_body.push(node.body.clone().unwrap_or_default());
            }
            NodeKind::DomainEvent => {
                domain_event_fqn.push(fqn.clone());
                domain_event_name.push(node.name.clone().unwrap_or_default());
                domain_event_body.push(node.body.clone().unwrap_or_default());
            }
            NodeKind::DomainProcess => {
                domain_process_fqn.push(fqn.clone());
                domain_process_name.push(node.name.clone().unwrap_or_default());
                domain_process_body.push(node.body.clone().unwrap_or_default());
            }
            NodeKind::DomainRule => {
                domain_rule_fqn.push(fqn.clone());
                domain_rule_name.push(node.name.clone().unwrap_or_default());
                domain_rule_body.push(node.body.clone().unwrap_or_default());
            }
            NodeKind::Actor => {
                actor_fqn.push(fqn.clone());
                actor_name.push(node.name.clone().unwrap_or_default());
                actor_body.push(node.body.clone().unwrap_or_default());
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
            NodeKind::Invariant => {
                invariant_fqn.push(fqn.clone());
                invariant_title.push(node.title.clone().unwrap_or_default());
                invariant_body.push(node.body.clone().unwrap_or_default());
                invariant_category.push(node.category.clone().unwrap_or_default());
                invariant_scope.push(node.scope.clone().unwrap_or_default());
                invariant_status.push(node.status.clone().unwrap_or_default());
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
        &dir.join("non_goal.parquet"),
        &[
            ("fqn", Col::Str(nongoal_fqn)),
            ("body", Col::Str(nongoal_body)),
        ],
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
        &dir.join("domain.parquet"),
        &[
            ("fqn", Col::Str(domain_fqn)),
            ("name", Col::Str(domain_name)),
            ("body", Col::Str(domain_body)),
        ],
    )?;
    write_parquet(
        &dir.join("subdomain.parquet"),
        &[
            ("fqn", Col::Str(subdomain_fqn)),
            ("name", Col::Str(subdomain_name)),
            ("kind", Col::Str(subdomain_kind)),
            ("body", Col::Str(subdomain_body)),
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
        &dir.join("value_object.parquet"),
        &[
            ("fqn", Col::Str(value_object_fqn)),
            ("name", Col::Str(value_object_name)),
            ("body", Col::Str(value_object_body)),
        ],
    )?;
    write_parquet(
        &dir.join("aggregate.parquet"),
        &[
            ("fqn", Col::Str(aggregate_fqn)),
            ("name", Col::Str(aggregate_name)),
            ("root", Col::Str(aggregate_root)),
            ("body", Col::Str(aggregate_body)),
        ],
    )?;
    write_parquet(
        &dir.join("domain_event.parquet"),
        &[
            ("fqn", Col::Str(domain_event_fqn)),
            ("name", Col::Str(domain_event_name)),
            ("body", Col::Str(domain_event_body)),
        ],
    )?;
    write_parquet(
        &dir.join("domain_process.parquet"),
        &[
            ("fqn", Col::Str(domain_process_fqn)),
            ("name", Col::Str(domain_process_name)),
            ("body", Col::Str(domain_process_body)),
        ],
    )?;
    write_parquet(
        &dir.join("domain_rule.parquet"),
        &[
            ("fqn", Col::Str(domain_rule_fqn)),
            ("name", Col::Str(domain_rule_name)),
            ("body", Col::Str(domain_rule_body)),
        ],
    )?;
    write_parquet(
        &dir.join("actor.parquet"),
        &[
            ("fqn", Col::Str(actor_fqn)),
            ("name", Col::Str(actor_name)),
            ("body", Col::Str(actor_body)),
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
        &dir.join("invariant.parquet"),
        &[
            ("fqn", Col::Str(invariant_fqn)),
            ("title", Col::Str(invariant_title)),
            ("body", Col::Str(invariant_body)),
            ("category", Col::Str(invariant_category)),
            ("scope", Col::Str(invariant_scope)),
            ("status", Col::Str(invariant_status)),
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
            "Drives" => {
                for (a, b) in &graph.drives {
                    if graph.nodes[a].kind == from && graph.nodes[b].kind == to {
                        fa.push(a.clone());
                        fb.push(b.clone());
                    }
                }
            }
            "Requires" => {
                for (a, b) in &graph.requires {
                    if graph.nodes[a].kind == from && graph.nodes[b].kind == to {
                        fa.push(a.clone());
                        fb.push(b.clone());
                    }
                }
            }
            "Realises" => {
                for (a, b) in &graph.realises {
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
            "ImplementedBy" => {
                for (a, b) in &graph.implemented_by {
                    if graph.nodes[a].kind == from && graph.nodes[b].kind == to {
                        fa.push(a.clone());
                        fb.push(b.clone());
                    }
                }
            }
            "GuardedBy" => {
                for (a, b) in &graph.guarded_by {
                    if graph.nodes[a].kind == from && graph.nodes[b].kind == to {
                        fa.push(a.clone());
                        fb.push(b.clone());
                    }
                }
            }
            "Checks" => {
                for (a, b) in &graph.checks {
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
        // Tier-1/2/3 hierarchy (GraphModel-SPEC.md; PHASE_01). The DDD chain is
        // strict: `Domain ⊃ Subdomain ⊃ Aggregate ⊃ Entity/ValueObject` — an
        // Aggregate belongs under a Subdomain, never directly under a Domain.
        // Stakeholder/Domain/System hang under the Spec root (the top of the
        // proposed-reality tree).
        (Spec, Stakeholder),
        (Spec, Domain),
        (Spec, System),
        (Domain, Subdomain),
        (Domain, DomainEvent),
        (Domain, DomainProcess),
        (Domain, DomainRule),
        (Domain, Actor),
        (Subdomain, Aggregate),
        (Aggregate, Entity),
        (Aggregate, ValueObject),
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

/// Whether the `Contains` rel table declares an edge between the two node
/// kinds — the single source of truth (via [`contains_pairs`]) for the
/// `spec add <tier> --parent` allow-list. A pair outside it is rejected at
/// write time (`add_tier_node`), never silently projected away at re-ingest.
pub fn contains_pair_allowed(a: NodeKind, b: NodeKind) -> bool {
    contains_pairs().contains(&(a, b))
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
        Spec,
        Requirement,
        Phase,
        Decision,
        NonGoal,
        AcceptanceCriterion,
        VerificationItem,
        Plan,
        PlanPhase,
        Task,
        Stakeholder,
        Domain,
        Subdomain,
        Entity,
        ValueObject,
        Aggregate,
        DomainEvent,
        DomainProcess,
        DomainRule,
        Actor,
        System,
        Container,
        Component,
    ] {
        v.push(("Details", Note, to));
    }
    for to in [
        Module,
        Function,
        Struct,
        File,
        Spec,
        Requirement,
        Phase,
        Decision,
        NonGoal,
        AcceptanceCriterion,
        VerificationItem,
        Plan,
        PlanPhase,
        Task,
        Stakeholder,
        Domain,
        Subdomain,
        Entity,
        ValueObject,
        Aggregate,
        DomainEvent,
        DomainProcess,
        DomainRule,
        Actor,
        System,
        Container,
        Component,
    ] {
        v.push(("Reviews", Feedback, to));
    }
    // Anchors: Requirement → real code (resolved) or a proposed Solution node
    // (pending, the tier-3 placeholder of the plan bridge) — the placeholder
    // node is gone (GraphModel-SPEC.md); Task → the real code it touches.
    for to in [Function, Struct, File, Module, System, Container, Component] {
        v.push(("Anchors", Requirement, to));
    }
    for to in [Function, Struct, File, Module] {
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
    // Builds: Task → the planned Implementation node it creates (was Task →
    // the placeholder node). A scan that realizes the planned node keeps the
    // edge pointing at the same FQN, now the real code.
    for to in [Module, File, Struct, Function] {
        v.push(("Builds", Task, to));
    }
    // Spine edges (GraphModel-SPEC.md; PHASE_01).
    v.push(("Drives", Requirement, Domain));
    v.push(("Requires", Requirement, Domain));
    for to in [System, Container, Component] {
        v.push(("Realises", Domain, to));
        v.push(("Represents", Domain, to));
        v.push(("ImplementedBy", to, Module));
        v.push(("ImplementedBy", to, File));
        v.push(("ImplementedBy", to, Struct));
        v.push(("ImplementedBy", to, Function));
    }
    // Invariant edges (Invariants-SPEC.md; PHASE_02): GuardedBy artifact →
    // Invariant (every guardable kind), Checks Feedback → Invariant.
    for from in [
        Module,
        File,
        Struct,
        Function,
        Spec,
        Requirement,
        Phase,
        Decision,
        NonGoal,
        AcceptanceCriterion,
        VerificationItem,
        Plan,
        PlanPhase,
        Task,
        Stakeholder,
        Domain,
        Subdomain,
        Entity,
        ValueObject,
        Aggregate,
        DomainEvent,
        DomainProcess,
        DomainRule,
        Actor,
        System,
        Container,
        Component,
    ] {
        v.push(("GuardedBy", from, Invariant));
    }
    v.push(("Checks", Feedback, Invariant));
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
        NodeKind::NonGoal => "non_goal",
        NodeKind::AcceptanceCriterion => "acceptance_criterion",
        NodeKind::VerificationItem => "verification_item",
        NodeKind::Note => "note",
        NodeKind::Feedback => "feedback",
        NodeKind::Plan => "plan",
        NodeKind::PlanPhase => "plan_phase",
        NodeKind::Task => "task",
        NodeKind::Scan => "scan",
        NodeKind::Stakeholder => "stakeholder",
        NodeKind::Domain => "domain",
        NodeKind::Subdomain => "subdomain",
        NodeKind::Entity => "entity",
        NodeKind::ValueObject => "value_object",
        NodeKind::Aggregate => "aggregate",
        NodeKind::DomainEvent => "domain_event",
        NodeKind::DomainProcess => "domain_process",
        NodeKind::DomainRule => "domain_rule",
        NodeKind::Actor => "actor",
        NodeKind::System => "system",
        NodeKind::Container => "container",
        NodeKind::Component => "component",
        NodeKind::Invariant => "invariant",
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

/// The node labels a Note may attach to via a `Details` edge — the `FROM Note
/// TO …` targets of the Details rel table (SPEC R2/R21). The single source of
/// truth for the `add_note --on` allow-list: Note/Feedback are excluded by
/// construction (a note cannot attach to another note or a review item — the
/// DB has no rel-table pair for it).
pub fn details_target_labels() -> &'static [&'static str] {
    static LABELS: std::sync::OnceLock<Vec<&'static str>> = std::sync::OnceLock::new();
    LABELS.get_or_init(|| {
        spec_rel_pairs()
            .into_iter()
            .filter(|(t, from, _)| *t == "Details" && *from == NodeKind::Note)
            .map(|(_, _, to)| label_of(to))
            .collect()
    })
}

/// The node labels an artifact may be guarded onto an Invariant via a
/// `GuardedBy` edge — the `FROM … TO Invariant` targets of the GuardedBy rel
/// table (Invariants-SPEC.md). The single source of truth for the
/// `invariant add --guard` allow-list: Note/Feedback/UnresolvedTarget/Scan/
/// Invariant are excluded by construction (a guard must be an artifact the
/// invariant constrains — not a note, review item, unresolved reference, scan
/// record, or another invariant — and the DB has no rel-table pair for it).
pub fn guarded_by_from_labels() -> &'static [&'static str] {
    static LABELS: std::sync::OnceLock<Vec<&'static str>> = std::sync::OnceLock::new();
    LABELS.get_or_init(|| {
        spec_rel_pairs()
            .into_iter()
            .filter(|(t, _, to)| *t == "GuardedBy" && *to == NodeKind::Invariant)
            .map(|(_, from, _)| label_of(from))
            .collect()
    })
}

/// The node labels a `--implemented-by` target may carry — the `FROM … TO …`
/// targets of the ImplementedBy rel table (Solution → Implementation,
/// GraphModel-SPEC.md). The single source of truth for the `spine
/// --implemented-by` target allow-list: UnresolvedTarget and every spec/plan
/// label are excluded by construction (an edge can only land on implementation
/// code, and the DB has no rel-table pair for the rest).
pub fn implemented_by_to_labels() -> &'static [&'static str] {
    static LABELS: std::sync::OnceLock<Vec<&'static str>> = std::sync::OnceLock::new();
    LABELS.get_or_init(|| {
        spec_rel_pairs()
            .into_iter()
            .filter(|(t, _, _)| *t == "ImplementedBy")
            .map(|(_, _, to)| label_of(to))
            .collect()
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
        "Spec",
        "Requirement",
        "Phase",
        "Decision",
        "NonGoal",
        "AcceptanceCriterion",
        "VerificationItem",
        "Note",
        "Feedback",
        "Plan",
        "PlanPhase",
        "Task",
        "Stakeholder",
        "Domain",
        "Subdomain",
        "Entity",
        "ValueObject",
        "Aggregate",
        "DomainEvent",
        "DomainProcess",
        "DomainRule",
        "Actor",
        "System",
        "Container",
        "Component",
        "Invariant",
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
        NodeKind::Spec => "Spec",
        NodeKind::Requirement => "Requirement",
        NodeKind::Phase => "Phase",
        NodeKind::Decision => "Decision",
        NodeKind::NonGoal => "NonGoal",
        NodeKind::AcceptanceCriterion => "AcceptanceCriterion",
        NodeKind::VerificationItem => "VerificationItem",
        NodeKind::Note => "Note",
        NodeKind::Feedback => "Feedback",
        NodeKind::Plan => "Plan",
        NodeKind::PlanPhase => "PlanPhase",
        NodeKind::Task => "Task",
        NodeKind::Scan => "Scan",
        NodeKind::Stakeholder => "Stakeholder",
        NodeKind::Domain => "Domain",
        NodeKind::Subdomain => "Subdomain",
        NodeKind::Entity => "Entity",
        NodeKind::ValueObject => "ValueObject",
        NodeKind::Aggregate => "Aggregate",
        NodeKind::DomainEvent => "DomainEvent",
        NodeKind::DomainProcess => "DomainProcess",
        NodeKind::DomainRule => "DomainRule",
        NodeKind::Actor => "Actor",
        NodeKind::System => "System",
        NodeKind::Container => "Container",
        NodeKind::Component => "Component",
        NodeKind::Invariant => "Invariant",
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
    conn.query("CREATE NODE TABLE Spec(fqn STRING PRIMARY KEY, title STRING, goal STRING)")?;
    conn.query(
        "CREATE NODE TABLE Requirement(fqn STRING PRIMARY KEY, id STRING, title STRING, body STRING, feature STRING)",
    )?;
    conn.query("CREATE NODE TABLE Phase(fqn STRING PRIMARY KEY, number INT64, title STRING)")?;
    conn.query("CREATE NODE TABLE Decision(fqn STRING PRIMARY KEY, id STRING, summary STRING)")?;
    conn.query("CREATE NODE TABLE NonGoal(fqn STRING PRIMARY KEY, body STRING)")?;
    conn.query("CREATE NODE TABLE AcceptanceCriterion(fqn STRING PRIMARY KEY, body STRING)")?;
    conn.query("CREATE NODE TABLE VerificationItem(fqn STRING PRIMARY KEY, body STRING)")?;
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
    conn.query(
        "CREATE NODE TABLE Stakeholder(fqn STRING PRIMARY KEY, name STRING, body STRING)",
    )?;
    conn.query(
        "CREATE NODE TABLE Domain(fqn STRING PRIMARY KEY, name STRING, body STRING)",
    )?;
    conn.query(
        "CREATE NODE TABLE Subdomain(fqn STRING PRIMARY KEY, name STRING, kind STRING, body STRING)",
    )?;
    conn.query(
        "CREATE NODE TABLE Entity(fqn STRING PRIMARY KEY, name STRING, body STRING)",
    )?;
    conn.query(
        "CREATE NODE TABLE ValueObject(fqn STRING PRIMARY KEY, name STRING, body STRING)",
    )?;
    conn.query(
        "CREATE NODE TABLE Aggregate(fqn STRING PRIMARY KEY, name STRING, root STRING, body STRING)",
    )?;
    conn.query(
        "CREATE NODE TABLE DomainEvent(fqn STRING PRIMARY KEY, name STRING, body STRING)",
    )?;
    conn.query(
        "CREATE NODE TABLE DomainProcess(fqn STRING PRIMARY KEY, name STRING, body STRING)",
    )?;
    conn.query(
        "CREATE NODE TABLE DomainRule(fqn STRING PRIMARY KEY, name STRING, body STRING)",
    )?;
    conn.query(
        "CREATE NODE TABLE Actor(fqn STRING PRIMARY KEY, name STRING, body STRING)",
    )?;
    conn.query(
        "CREATE NODE TABLE System(fqn STRING PRIMARY KEY, name STRING, body STRING)",
    )?;
    conn.query(
        "CREATE NODE TABLE Container(fqn STRING PRIMARY KEY, name STRING, kind STRING, body STRING)",
    )?;
    conn.query(
        "CREATE NODE TABLE Component(fqn STRING PRIMARY KEY, name STRING, body STRING)",
    )?;
    conn.query(
        "CREATE NODE TABLE Invariant(fqn STRING PRIMARY KEY, title STRING, body STRING, category STRING, scope STRING, status STRING)",
    )?;
    conn.query(
        "CREATE REL TABLE Contains(FROM Module TO Module, FROM Module TO File, FROM File TO Struct, FROM File TO Function, FROM Struct TO Struct, FROM Struct TO Function, FROM Spec TO Requirement, FROM Spec TO Phase, FROM Phase TO Requirement, FROM Spec TO Decision, FROM Spec TO NonGoal, FROM Spec TO AcceptanceCriterion, FROM Spec TO VerificationItem, FROM Plan TO PlanPhase, FROM PlanPhase TO Task, FROM PlanPhase TO AcceptanceCriterion, FROM PlanPhase TO VerificationItem, FROM Spec TO Stakeholder, FROM Spec TO Domain, FROM Spec TO System, FROM Domain TO Subdomain, FROM Domain TO DomainEvent, FROM Domain TO DomainProcess, FROM Domain TO DomainRule, FROM Domain TO Actor, FROM Subdomain TO Aggregate, FROM Aggregate TO Entity, FROM Aggregate TO ValueObject, FROM System TO Container, FROM Container TO Component)",
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
        "CREATE REL TABLE Details(FROM Note TO Module, FROM Note TO Function, FROM Note TO Struct, FROM Note TO File, FROM Note TO Spec, FROM Note TO Requirement, FROM Note TO Phase, FROM Note TO Decision, FROM Note TO NonGoal, FROM Note TO AcceptanceCriterion, FROM Note TO VerificationItem, FROM Note TO Plan, FROM Note TO PlanPhase, FROM Note TO Task, FROM Note TO Stakeholder, FROM Note TO Domain, FROM Note TO Subdomain, FROM Note TO Entity, FROM Note TO ValueObject, FROM Note TO Aggregate, FROM Note TO DomainEvent, FROM Note TO DomainProcess, FROM Note TO DomainRule, FROM Note TO Actor, FROM Note TO System, FROM Note TO Container, FROM Note TO Component)",
    )?;
    conn.query(
        "CREATE REL TABLE Reviews(FROM Feedback TO Module, FROM Feedback TO Function, FROM Feedback TO Struct, FROM Feedback TO File, FROM Feedback TO Spec, FROM Feedback TO Requirement, FROM Feedback TO Phase, FROM Feedback TO Decision, FROM Feedback TO NonGoal, FROM Feedback TO AcceptanceCriterion, FROM Feedback TO VerificationItem, FROM Feedback TO Plan, FROM Feedback TO PlanPhase, FROM Feedback TO Task, FROM Feedback TO Stakeholder, FROM Feedback TO Domain, FROM Feedback TO Subdomain, FROM Feedback TO Entity, FROM Feedback TO ValueObject, FROM Feedback TO Aggregate, FROM Feedback TO DomainEvent, FROM Feedback TO DomainProcess, FROM Feedback TO DomainRule, FROM Feedback TO Actor, FROM Feedback TO System, FROM Feedback TO Container, FROM Feedback TO Component)",
    )?;
    conn.query("CREATE REL TABLE DependsOn(FROM Requirement TO Requirement)")?;
    conn.query("CREATE REL TABLE Gates(FROM Phase TO Phase, FROM PlanPhase TO PlanPhase)")?;
    conn.query("CREATE REL TABLE SpecDependsOn(FROM Spec TO Spec)")?;
    conn.query(
        "CREATE REL TABLE Anchors(FROM Requirement TO Function, FROM Requirement TO Struct, FROM Requirement TO File, FROM Requirement TO Module, FROM Requirement TO System, FROM Requirement TO Container, FROM Requirement TO Component, FROM Task TO Function, FROM Task TO Struct, FROM Task TO File, FROM Task TO Module)",
    )?;
    conn.query(
        "CREATE REL TABLE Implements(FROM Function TO Requirement, FROM Struct TO Requirement, FROM File TO Requirement)",
    )?;
    conn.query("CREATE REL TABLE Satisfies(FROM PlanPhase TO Requirement)")?;
    conn.query(
        "CREATE REL TABLE Builds(FROM Task TO Module, FROM Task TO File, FROM Task TO Struct, FROM Task TO Function)",
    )?;
    conn.query(
        "CREATE REL TABLE Drives(FROM Requirement TO Domain)",
    )?;
    conn.query(
        "CREATE REL TABLE Requires(FROM Requirement TO Domain)",
    )?;
    conn.query(
        "CREATE REL TABLE Realises(FROM Domain TO System, FROM Domain TO Container, FROM Domain TO Component)",
    )?;
    conn.query(
        "CREATE REL TABLE Represents(FROM Domain TO System, FROM Domain TO Container, FROM Domain TO Component)",
    )?;
    conn.query(
        "CREATE REL TABLE ImplementedBy(FROM System TO Module, FROM System TO File, FROM System TO Struct, FROM System TO Function, FROM Container TO Module, FROM Container TO File, FROM Container TO Struct, FROM Container TO Function, FROM Component TO Module, FROM Component TO File, FROM Component TO Struct, FROM Component TO Function)",
    )?;
    conn.query(
        "CREATE REL TABLE GuardedBy(FROM Module TO Invariant, FROM File TO Invariant, FROM Struct TO Invariant, FROM Function TO Invariant, FROM Spec TO Invariant, FROM Requirement TO Invariant, FROM Phase TO Invariant, FROM Decision TO Invariant, FROM NonGoal TO Invariant, FROM AcceptanceCriterion TO Invariant, FROM VerificationItem TO Invariant, FROM Plan TO Invariant, FROM PlanPhase TO Invariant, FROM Task TO Invariant, FROM Stakeholder TO Invariant, FROM Domain TO Invariant, FROM Subdomain TO Invariant, FROM Entity TO Invariant, FROM ValueObject TO Invariant, FROM Aggregate TO Invariant, FROM DomainEvent TO Invariant, FROM DomainProcess TO Invariant, FROM DomainRule TO Invariant, FROM Actor TO Invariant, FROM System TO Invariant, FROM Container TO Invariant, FROM Component TO Invariant)",
    )?;
    conn.query("CREATE REL TABLE Checks(FROM Feedback TO Invariant)")?;
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
        format!(r#"COPY Spec FROM "{}""#, p("spec.parquet")),
        format!(r#"COPY Requirement FROM "{}""#, p("requirement.parquet")),
        format!(r#"COPY Phase FROM "{}""#, p("phase.parquet")),
        format!(r#"COPY Decision FROM "{}""#, p("decision.parquet")),
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
            r#"COPY Stakeholder FROM "{}""#,
            p("stakeholder.parquet")
        ),
        format!(r#"COPY Domain FROM "{}""#, p("domain.parquet")),
        format!(r#"COPY Subdomain FROM "{}""#, p("subdomain.parquet")),
        format!(r#"COPY Entity FROM "{}""#, p("entity.parquet")),
        format!(
            r#"COPY ValueObject FROM "{}""#,
            p("value_object.parquet")
        ),
        format!(r#"COPY Aggregate FROM "{}""#, p("aggregate.parquet")),
        format!(
            r#"COPY DomainEvent FROM "{}""#,
            p("domain_event.parquet")
        ),
        format!(
            r#"COPY DomainProcess FROM "{}""#,
            p("domain_process.parquet")
        ),
        format!(
            r#"COPY DomainRule FROM "{}""#,
            p("domain_rule.parquet")
        ),
        format!(r#"COPY Actor FROM "{}""#, p("actor.parquet")),
        format!(r#"COPY System FROM "{}""#, p("system.parquet")),
        format!(r#"COPY Container FROM "{}""#, p("container.parquet")),
        format!(r#"COPY Component FROM "{}""#, p("component.parquet")),
        format!(r#"COPY Invariant FROM "{}""#, p("invariant.parquet")),
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
    Domain {
        fqn: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        name: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        body: String,
    },
    Subdomain {
        fqn: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        name: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        kind: String,
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
    ValueObject {
        fqn: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        name: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        body: String,
    },
    Aggregate {
        fqn: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        name: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        root: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        body: String,
    },
    DomainEvent {
        fqn: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        name: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        body: String,
    },
    DomainProcess {
        fqn: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        name: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        body: String,
    },
    DomainRule {
        fqn: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        name: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        body: String,
    },
    Actor {
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
    Invariant {
        fqn: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        title: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        body: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        category: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        scope: String,
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
    Drives {
        from: String,
        to: String,
    },
    Requires {
        from: String,
        to: String,
    },
    Realises {
        from: String,
        to: String,
    },
    Represents {
        from: String,
        to: String,
    },
    ImplementedBy {
        from: String,
        to: String,
    },
    GuardedBy {
        from: String,
        to: String,
    },
    Checks {
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
            NodeKind::Domain => Export::Domain {
                fqn: fqn.clone(),
                name: node.name.clone().unwrap_or_default(),
                body: node.body.clone().unwrap_or_default(),
            },
            NodeKind::Subdomain => Export::Subdomain {
                fqn: fqn.clone(),
                name: node.name.clone().unwrap_or_default(),
                kind: node.sub_kind.clone().unwrap_or_default(),
                body: node.body.clone().unwrap_or_default(),
            },
            NodeKind::Entity => Export::Entity {
                fqn: fqn.clone(),
                name: node.name.clone().unwrap_or_default(),
                body: node.body.clone().unwrap_or_default(),
            },
            NodeKind::ValueObject => Export::ValueObject {
                fqn: fqn.clone(),
                name: node.name.clone().unwrap_or_default(),
                body: node.body.clone().unwrap_or_default(),
            },
            NodeKind::Aggregate => Export::Aggregate {
                fqn: fqn.clone(),
                name: node.name.clone().unwrap_or_default(),
                root: node.root.clone().unwrap_or_default(),
                body: node.body.clone().unwrap_or_default(),
            },
            NodeKind::DomainEvent => Export::DomainEvent {
                fqn: fqn.clone(),
                name: node.name.clone().unwrap_or_default(),
                body: node.body.clone().unwrap_or_default(),
            },
            NodeKind::DomainProcess => Export::DomainProcess {
                fqn: fqn.clone(),
                name: node.name.clone().unwrap_or_default(),
                body: node.body.clone().unwrap_or_default(),
            },
            NodeKind::DomainRule => Export::DomainRule {
                fqn: fqn.clone(),
                name: node.name.clone().unwrap_or_default(),
                body: node.body.clone().unwrap_or_default(),
            },
            NodeKind::Actor => Export::Actor {
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
            NodeKind::Invariant => Export::Invariant {
                fqn: fqn.clone(),
                title: node.title.clone().unwrap_or_default(),
                body: node.body.clone().unwrap_or_default(),
                category: node.category.clone().unwrap_or_default(),
                scope: node.scope.clone().unwrap_or_default(),
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
    for (a, b) in &graph.drives {
        write_line(
            &mut w,
            &Export::Drives {
                from: a.clone(),
                to: b.clone(),
            },
        )?;
    }
    for (a, b) in &graph.requires {
        write_line(
            &mut w,
            &Export::Requires {
                from: a.clone(),
                to: b.clone(),
            },
        )?;
    }
    for (a, b) in &graph.realises {
        write_line(
            &mut w,
            &Export::Realises {
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
    for (a, b) in &graph.implemented_by {
        write_line(
            &mut w,
            &Export::ImplementedBy {
                from: a.clone(),
                to: b.clone(),
            },
        )?;
    }
    for (a, b) in &graph.guarded_by {
        write_line(
            &mut w,
            &Export::GuardedBy {
                from: a.clone(),
                to: b.clone(),
            },
        )?;
    }
    for (a, b) in &graph.checks {
        write_line(
            &mut w,
            &Export::Checks {
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
                "spec" => {
                    g.nodes.insert(
                        s("fqn"),
                        Node {
                            kind: NodeKind::Spec,
                            title: o("title"),
                            goal: o("goal"),
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
                "phase" => {
                    g.nodes.insert(
                        s("fqn"),
                        Node {
                            kind: NodeKind::Phase,
                            number: v.get("number").and_then(|x| x.as_u64()).map(|x| x as u32),
                            title: o("title"),
                            ..Node::default()
                        },
                    );
                }
                "decision" => {
                    g.nodes.insert(
                        s("fqn"),
                        Node {
                            kind: NodeKind::Decision,
                            id: o("id"),
                            summary: o("summary"),
                            ..Node::default()
                        },
                    );
                }
                "non_goal" => {
                    g.nodes.insert(
                        s("fqn"),
                        Node {
                            kind: NodeKind::NonGoal,
                            body: o("body"),
                            ..Node::default()
                        },
                    );
                }
                "acceptance_criterion" => {
                    g.nodes.insert(
                        s("fqn"),
                        Node {
                            kind: NodeKind::AcceptanceCriterion,
                            body: o("body"),
                            ..Node::default()
                        },
                    );
                }
                "verification_item" => {
                    g.nodes.insert(
                        s("fqn"),
                        Node {
                            kind: NodeKind::VerificationItem,
                            body: o("body"),
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
                "domain" => {
                    g.nodes.insert(
                        s("fqn"),
                        Node {
                            kind: NodeKind::Domain,
                            name: o("name"),
                            body: o("body"),
                            ..Node::default()
                        },
                    );
                }
                "subdomain" => {
                    g.nodes.insert(
                        s("fqn"),
                        Node {
                            kind: NodeKind::Subdomain,
                            name: o("name"),
                            sub_kind: o("kind"),
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
                "value_object" => {
                    g.nodes.insert(
                        s("fqn"),
                        Node {
                            kind: NodeKind::ValueObject,
                            name: o("name"),
                            body: o("body"),
                            ..Node::default()
                        },
                    );
                }
                "aggregate" => {
                    g.nodes.insert(
                        s("fqn"),
                        Node {
                            kind: NodeKind::Aggregate,
                            name: o("name"),
                            root: o("root"),
                            body: o("body"),
                            ..Node::default()
                        },
                    );
                }
                "domain_event" => {
                    g.nodes.insert(
                        s("fqn"),
                        Node {
                            kind: NodeKind::DomainEvent,
                            name: o("name"),
                            body: o("body"),
                            ..Node::default()
                        },
                    );
                }
                "domain_process" => {
                    g.nodes.insert(
                        s("fqn"),
                        Node {
                            kind: NodeKind::DomainProcess,
                            name: o("name"),
                            body: o("body"),
                            ..Node::default()
                        },
                    );
                }
                "domain_rule" => {
                    g.nodes.insert(
                        s("fqn"),
                        Node {
                            kind: NodeKind::DomainRule,
                            name: o("name"),
                            body: o("body"),
                            ..Node::default()
                        },
                    );
                }
                "actor" => {
                    g.nodes.insert(
                        s("fqn"),
                        Node {
                            kind: NodeKind::Actor,
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
                "invariant" => {
                    g.nodes.insert(
                        s("fqn"),
                        Node {
                            kind: NodeKind::Invariant,
                            title: o("title"),
                            body: o("body"),
                            category: o("category"),
                            scope: o("scope"),
                            status: o("status"),
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
                    g.unresolved_calls.insert((s("from"), s("to"), s("target_type")));
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
                "spec_depends" => {
                    g.spec_depends.insert((s("from"), s("to")));
                }
                "anchors" => {
                    g.anchors.insert((s("from"), s("to")));
                }
                "implements" => {
                    g.implements.insert((s("from"), s("to")));
                }
                "satisfies" => {
                    g.satisfies.insert((s("from"), s("to")));
                }
                "builds" => {
                    g.builds.insert((s("from"), s("to")));
                }
                "drives" => {
                    g.drives.insert((s("from"), s("to")));
                }
                "requires" => {
                    g.requires.insert((s("from"), s("to")));
                }
                "realises" => {
                    g.realises.insert((s("from"), s("to")));
                }
                "represents" => {
                    g.represents.insert((s("from"), s("to")));
                }
                "implemented_by" => {
                    g.implemented_by.insert((s("from"), s("to")));
                }
                "guarded_by" => {
                    g.guarded_by.insert((s("from"), s("to")));
                }
                "checks" => {
                    g.checks.insert((s("from"), s("to")));
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
            "foo/spec",
            Node {
                title: Some("Widget timer".to_string()),
                goal: Some("Let widgets time out".to_string()),
                ..sp(NodeKind::Spec)
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
            "foo/spec.R2",
            Node {
                id: Some("R2".to_string()),
                title: Some("Expiry".to_string()),
                ..sp(NodeKind::Requirement)
            },
        );
        n(
            "foo/spec.phase-1",
            Node {
                number: Some(1),
                title: Some("Core".to_string()),
                ..sp(NodeKind::Phase)
            },
        );
        n(
            "foo/spec.phase-2",
            Node {
                number: Some(2),
                title: Some("Polish".to_string()),
                ..sp(NodeKind::Phase)
            },
        );
        n(
            "foo/spec.decision-d1",
            Node {
                id: Some("d1".to_string()),
                summary: Some("Timeout in wall clock".to_string()),
                ..sp(NodeKind::Decision)
            },
        );
        n(
            "foo/spec.ng1",
            Node {
                body: Some("No daemon".to_string()),
                ..sp(NodeKind::NonGoal)
            },
        );
        n(
            "foo/spec.ac1",
            Node {
                body: Some("Timer fires once".to_string()),
                ..sp(NodeKind::AcceptanceCriterion)
            },
        );
        n(
            "foo/spec.vi1",
            Node {
                body: Some("cargo test green".to_string()),
                ..sp(NodeKind::VerificationItem)
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
            "other/spec",
            Node {
                title: Some("Other".to_string()),
                ..sp(NodeKind::Spec)
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
                tier: Some("".to_string()),
                status: Some("pending".to_string()),
                ..sp(NodeKind::Task)
            },
        );

        g.contains.extend([
            ("foo/spec".into(), "foo/spec.R1".into()),
            ("foo/spec".into(), "foo/spec.phase-1".into()),
            ("foo/spec".into(), "foo/spec.phase-2".into()),
            (
                "foo/spec".into(),
                "foo/spec.decision-d1".into(),
            ),
            ("foo/spec".into(), "foo/spec.ng1".into()),
            ("foo/spec".into(), "foo/spec.ac1".into()),
            ("foo/spec".into(), "foo/spec.vi1".into()),
            (
                "foo/spec.phase-1".into(),
                "foo/spec.R1".into(),
            ),
            ("foo/plan".into(), "foo/plan.phase-1".into()),
            (
                "foo/plan.phase-1".into(),
                "foo/plan.phase-1.task-1".into(),
            ),
        ]);
        g.details
            .insert(("foo/note-1".into(), "foo/spec".into()));
        g.reviews
            .insert(("foo/feedback-1".into(), "foo/spec.R1".into()));
        g.depends_on
            .insert(("foo/spec.R2".into(), "foo/spec.R1".into()));
        g.gates.insert((
            "foo/spec.phase-2".into(),
            "foo/spec.phase-1".into(),
        ));
        g.spec_depends
            .insert(("foo/spec".into(), "other/spec".into()));
        g.anchors
            .insert(("foo/spec.R1".into(), "foo/gateway".into()));
        g.satisfies.insert((
            "foo/plan.phase-1".into(),
            "foo/spec.R1".into(),
        ));
        g.builds.insert((
            "foo/plan.phase-1.task-1".into(),
            "foo/gateway".into(),
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
            out.contains("foo/spec") && out.contains("Widget timer"),
            "spec rows: {out}"
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
            .query("MATCH (t:Task) RETURN t.kind, t.tier, t.status")
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
        assert!(
            out.contains("planned"),
            "planned struct row: {out}"
        );

        // Multi-pair Contains: Spec -> Requirement and Spec -> Phase.
        let out = conn
            .query("MATCH (s:Spec)-[:Contains]->(r:Requirement) RETURN r.fqn")
            .unwrap()
            .to_string();
        assert!(
            out.contains("foo/spec.R1"),
            "contains spec->req: {out}"
        );
        let out = conn
            .query("MATCH (s:Spec)-[:Contains]->(p:Phase) RETURN p.fqn")
            .unwrap()
            .to_string();
        assert!(
            out.contains("foo/spec.phase-1"),
            "contains spec->phase: {out}"
        );

        // Spec/plan rel tables.
        let out = conn
            .query("MATCH (:Note)-[:Details]->(s:Spec) RETURN s.fqn")
            .unwrap()
            .to_string();
        assert!(out.contains("foo/spec"), "details: {out}");
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
            .query("MATCH (a:Phase)-[:Gates]->(b:Phase) RETURN b.fqn")
            .unwrap()
            .to_string();
        assert!(out.contains("foo/spec.phase-1"), "gates: {out}");
        let out = conn
            .query("MATCH (a:Requirement)-[:Anchors]->(s:Struct) RETURN s.fqn")
            .unwrap()
            .to_string();
        assert!(out.contains("foo/gateway"), "anchors: {out}");
        let out = conn
            .query("MATCH (:PlanPhase)-[:Satisfies]->(r:Requirement) RETURN r.fqn")
            .unwrap()
            .to_string();
        assert!(out.contains("foo/spec.R1"), "satisfies: {out}");
        let out = conn
            .query("MATCH (:Task)-[:Builds]->(s:Struct) RETURN s.fqn")
            .unwrap()
            .to_string();
        assert!(out.contains("foo/gateway"), "builds: {out}");

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
        // REVIEW.md item: no export-then-reingest test covered the new tier
        // nodes and spine edges — PHASE_01's done gate ("JSONL → DB → JSONL
        // round-trip for each") had the write leg (`write_graph_jsonl`) and the
        // DB-query leg (`four_tier_spine_resolves_via_query`) tested separately,
        // but not the closed loop *through graph.jsonl*. Now the export is
        // genuinely round-trippable: write_graph_jsonl → read_graph_jsonl →
        // fresh DB, and every tier node + spine edge must survive. A real
        // `Scan` node round-trips too (scan_meta control record on line 1).
        let dir = std::env::temp_dir().join(format!("apg-test-spine-roundtrip-{}", std::process::id()));
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
        // Tier 1 + a requirement to anchor the spine.
        n(
            "foo/spec",
            Node {
                title: Some("Foo".to_string()),
                ..sp(NodeKind::Spec)
            },
        );
        n(
            "foo/spec.R1",
            Node {
                id: Some("R1".to_string()),
                title: Some("Users authenticate".to_string()),
                ..sp(NodeKind::Requirement)
            },
        );
        n(
            "foo/stakeholder.Ops",
            Node {
                name: Some("Ops".to_string()),
                ..sp(NodeKind::Stakeholder)
            },
        );
        // Tier 2 (DDD).
        n(
            "foo/domain.Auth",
            Node {
                name: Some("Auth".to_string()),
                ..sp(NodeKind::Domain)
            },
        );
        n(
            "foo/subdomain.Access",
            Node {
                name: Some("Access".to_string()),
                sub_kind: Some("core".to_string()),
                ..sp(NodeKind::Subdomain)
            },
        );
        n(
            "foo/aggregate.Session",
            Node {
                name: Some("Session".to_string()),
                root: Some("Session".to_string()),
                ..sp(NodeKind::Aggregate)
            },
        );
        n(
            "foo/entity.User",
            Node {
                name: Some("User".to_string()),
                ..sp(NodeKind::Entity)
            },
        );
        n(
            "foo/value-object.Email",
            Node {
                name: Some("Email".to_string()),
                ..sp(NodeKind::ValueObject)
            },
        );
        n(
            "foo/domain-rule.NoNegativeBalance",
            Node {
                name: Some("NoNegativeBalance".to_string()),
                ..sp(NodeKind::DomainRule)
            },
        );
        n(
            "foo/actor.Customer",
            Node {
                name: Some("Customer".to_string()),
                ..sp(NodeKind::Actor)
            },
        );
        // Tier 3 (C4).
        n(
            "foo/system.Platform",
            Node {
                name: Some("Platform".to_string()),
                ..sp(NodeKind::System)
            },
        );
        n(
            "foo/container.Api",
            Node {
                name: Some("Api".to_string()),
                sub_kind: Some("app".to_string()),
                ..sp(NodeKind::Container)
            },
        );
        n(
            "foo/component.Gateway",
            Node {
                name: Some("Gateway".to_string()),
                ..sp(NodeKind::Component)
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
        // The DDD/C4 hierarchy.
        g.contains.extend([
            ("foo/spec".into(), "foo/spec.R1".into()),
            ("foo/spec".into(), "foo/stakeholder.Ops".into()),
            ("foo/spec".into(), "foo/domain.Auth".into()),
            ("foo/spec".into(), "foo/system.Platform".into()),
            (
                "foo/domain.Auth".into(),
                "foo/subdomain.Access".into(),
            ),
            (
                "foo/domain.Auth".into(),
                "foo/domain-rule.NoNegativeBalance".into(),
            ),
            (
                "foo/domain.Auth".into(),
                "foo/actor.Customer".into(),
            ),
            (
                "foo/subdomain.Access".into(),
                "foo/aggregate.Session".into(),
            ),
            (
                "foo/aggregate.Session".into(),
                "foo/entity.User".into(),
            ),
            (
                "foo/aggregate.Session".into(),
                "foo/value-object.Email".into(),
            ),
            (
                "foo/system.Platform".into(),
                "foo/container.Api".into(),
            ),
            (
                "foo/container.Api".into(),
                "foo/component.Gateway".into(),
            ),
        ]);
        // The full five-edge spine.
        g.drives.insert((
            "foo/spec.R1".to_string(),
            "foo/domain.Auth".to_string(),
        ));
        g.requires.insert((
            "foo/spec.R1".to_string(),
            "foo/domain.Auth".to_string(),
        ));
        g.realises.insert((
            "foo/domain.Auth".to_string(),
            "foo/system.Platform".to_string(),
        ));
        g.represents.insert((
            "foo/domain.Auth".to_string(),
            "foo/system.Platform".to_string(),
        ));
        g.implemented_by.insert((
            "foo/component.Gateway".to_string(),
            "mod.Gateway".to_string(),
        ));

        let out_path = dir.join("graph.jsonl");
        write_graph_jsonl(&g, &out_path).unwrap();

        // Re-ingest the export: read_graph_jsonl rebuilds the graph (scan_meta
        // line 1 → the Scan node, tier nodes, Contains + every spine edge).
        let back = read_graph_jsonl(&out_path).unwrap();

        // Every node survives with its kind (a located code node keeps its span;
        // a location-less code node reads back without one).
        for (fqn, node) in &g.nodes {
            let seen = back.nodes.get(fqn).unwrap_or_else(|| panic!("{fqn} lost in round-trip"));
            assert_eq!(seen.kind, node.kind, "{fqn} kind");
        }
        assert_eq!(back.nodes.len(), g.nodes.len(), "node count");
        // Edge sets are identical — nothing projected away.
        assert_eq!(back.contains, g.contains, "contains edges");
        for (name, a, b) in [
            ("drives", &back.drives, &g.drives),
            ("requires", &back.requires, &g.requires),
            ("realises", &back.realises, &g.realises),
            ("represents", &back.represents, &g.represents),
            ("implemented_by", &back.implemented_by, &g.implemented_by),
            ("calls", &back.calls, &g.calls),
            ("uses", &back.uses, &g.uses),
            ("unresolved_uses", &back.unresolved_uses, &g.unresolved_uses),
        ] {
            assert_eq!(a, b, "{name} edges");
        }
        assert_eq!(back.unresolved_calls, g.unresolved_calls, "unresolved_calls edges");

        // The closed loop lands in a queryable DB: load the re-ingested graph
        // into a fresh DB and resolve every tier label + the spine end to end.
        let ldir = dir.join("load");
        std::fs::create_dir_all(&ldir).unwrap();
        build_load_files(&back, &ldir).unwrap();
        let db = Database::in_memory(SystemConfig::default()).unwrap();
        let conn = Connection::new(&db).unwrap();
        create_schema(&conn).unwrap();
        copy_from(&conn, &ldir).unwrap();

        for (label, fqn) in [
            ("Stakeholder", "foo/stakeholder.Ops"),
            ("Domain", "foo/domain.Auth"),
            ("Subdomain", "foo/subdomain.Access"),
            ("Aggregate", "foo/aggregate.Session"),
            ("Entity", "foo/entity.User"),
            ("ValueObject", "foo/value-object.Email"),
            ("DomainRule", "foo/domain-rule.NoNegativeBalance"),
            ("Actor", "foo/actor.Customer"),
            ("System", "foo/system.Platform"),
            ("Container", "foo/container.Api"),
            ("Component", "foo/component.Gateway"),
        ] {
            let out = conn
                .query(&format!("MATCH (n:{label} {{fqn: '{fqn}'}}) RETURN n.name"))
                .unwrap()
                .to_string();
            assert!(!out.contains("(empty)"), "{label} {fqn} missing after round-trip: {out}");
        }
        let out = conn
            .query(
                "MATCH (r:Requirement)-[:Drives]->(d:Domain)-[:Realises]->(s:System)-[:Contains]->(:Container)-[:Contains]->(c:Component)-[:ImplementedBy]->(impl) RETURN impl.fqn",
            )
            .unwrap()
            .to_string();
        assert!(out.contains("mod.Gateway"), "spine to code after round-trip: {out}");
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

    #[test]
    fn four_tier_spine_resolves_via_query() {
        // PHASE_01 done gate: a sample 4-tier spine
        // (Requirement → Domain → Solution → Implementation) survives the load
        // path and resolves via apg_query. Requirement Drives/Requires a Domain;
        // the Domain Realises/Represents a System; the System is ImplementedBy
        // code. The tier hierarchy (Spec ⊃ Stakeholder/Domain/System,
        // Domain ⊃ Subdomain/Events/…, System ⊃ Container ⊃ Component)
        // loads through the extended Contains table.
        let mut g = Graph::default();
        let sp = |kind: NodeKind| Node {
            kind,
            code_type: String::new(),
            ..Node::default()
        };
        let mut n = |fqn: &str, node: Node| {
            g.nodes.insert(fqn.to_string(), node);
        };
        // Tier 1.
        n(
            "foo/spec",
            Node {
                title: Some("Foo".to_string()),
                ..sp(NodeKind::Spec)
            },
        );
        n(
            "foo/spec.R1",
            Node {
                id: Some("R1".to_string()),
                title: Some("Users authenticate".to_string()),
                ..sp(NodeKind::Requirement)
            },
        );
        n(
            "foo/stakeholder.Ops",
            Node {
                name: Some("Ops".to_string()),
                ..sp(NodeKind::Stakeholder)
            },
        );
        // Tier 2 (DDD).
        n(
            "foo/domain.Auth",
            Node {
                name: Some("Auth".to_string()),
                ..sp(NodeKind::Domain)
            },
        );
        n(
            "foo/subdomain.Access",
            Node {
                name: Some("Access".to_string()),
                sub_kind: Some("core".to_string()),
                ..sp(NodeKind::Subdomain)
            },
        );
        n(
            "foo/aggregate.Session",
            Node {
                name: Some("Session".to_string()),
                root: Some("Session".to_string()),
                ..sp(NodeKind::Aggregate)
            },
        );
        n(
            "foo/entity.User",
            Node {
                name: Some("User".to_string()),
                ..sp(NodeKind::Entity)
            },
        );
        n(
            "foo/value-object.Email",
            Node {
                name: Some("Email".to_string()),
                ..sp(NodeKind::ValueObject)
            },
        );
        n(
            "foo/domain-event.UserLoggedIn",
            Node {
                name: Some("UserLoggedIn".to_string()),
                ..sp(NodeKind::DomainEvent)
            },
        );
        n(
            "foo/domain-process.Checkout",
            Node {
                name: Some("Checkout".to_string()),
                ..sp(NodeKind::DomainProcess)
            },
        );
        n(
            "foo/domain-rule.NoNegativeBalance",
            Node {
                name: Some("NoNegativeBalance".to_string()),
                ..sp(NodeKind::DomainRule)
            },
        );
        n(
            "foo/actor.Customer",
            Node {
                name: Some("Customer".to_string()),
                ..sp(NodeKind::Actor)
            },
        );
        // Tier 3 (C4).
        n(
            "foo/system.Platform",
            Node {
                name: Some("Platform".to_string()),
                ..sp(NodeKind::System)
            },
        );
        n(
            "foo/container.Api",
            Node {
                name: Some("Api".to_string()),
                sub_kind: Some("app".to_string()),
                ..sp(NodeKind::Container)
            },
        );
        n(
            "foo/component.Gateway",
            Node {
                name: Some("Gateway".to_string()),
                ..sp(NodeKind::Component)
            },
        );
        // Tier 4 (code, from a fixture scan).
        n("mod", sp(NodeKind::Module));
        n("mod.Gateway", sp(NodeKind::Struct));

        // Hierarchy.
        g.contains.extend([
            ("foo/spec".into(), "foo/spec.R1".into()),
            (
                "foo/spec".into(),
                "foo/stakeholder.Ops".into(),
            ),
            ("foo/spec".into(), "foo/domain.Auth".into()),
            ("foo/spec".into(), "foo/system.Platform".into()),
            (
                "foo/domain.Auth".into(),
                "foo/subdomain.Access".into(),
            ),
            (
                "foo/domain.Auth".into(),
                "foo/domain-event.UserLoggedIn".into(),
            ),
            (
                "foo/domain.Auth".into(),
                "foo/domain-process.Checkout".into(),
            ),
            (
                "foo/domain.Auth".into(),
                "foo/domain-rule.NoNegativeBalance".into(),
            ),
            (
                "foo/domain.Auth".into(),
                "foo/actor.Customer".into(),
            ),
            (
                "foo/subdomain.Access".into(),
                "foo/aggregate.Session".into(),
            ),
            (
                "foo/aggregate.Session".into(),
                "foo/entity.User".into(),
            ),
            (
                "foo/aggregate.Session".into(),
                "foo/value-object.Email".into(),
            ),
            (
                "foo/system.Platform".into(),
                "foo/container.Api".into(),
            ),
            (
                "foo/container.Api".into(),
                "foo/component.Gateway".into(),
            ),
        ]);
        // The spine.
        g.drives.insert((
            "foo/spec.R1".to_string(),
            "foo/domain.Auth".to_string(),
        ));
        g.realises.insert((
            "foo/domain.Auth".to_string(),
            "foo/system.Platform".to_string(),
        ));
        g.implemented_by.insert((
            "foo/component.Gateway".to_string(),
            "mod.Gateway".to_string(),
        ));

        let dir = std::env::temp_dir().join(format!("apg-test-spine-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        build_load_files(&g, &dir).unwrap();

        let db = Database::in_memory(SystemConfig::default()).unwrap();
        let conn = Connection::new(&db).unwrap();
        create_schema(&conn).unwrap();
        copy_from(&conn, &dir).unwrap();

        // Every tier node is queryable by its label.
        for (label, fqn) in [
            ("Stakeholder", "foo/stakeholder.Ops"),
            ("Domain", "foo/domain.Auth"),
            ("Subdomain", "foo/subdomain.Access"),
            ("Aggregate", "foo/aggregate.Session"),
            ("Entity", "foo/entity.User"),
            ("ValueObject", "foo/value-object.Email"),
            ("DomainEvent", "foo/domain-event.UserLoggedIn"),
            ("DomainProcess", "foo/domain-process.Checkout"),
            ("DomainRule", "foo/domain-rule.NoNegativeBalance"),
            ("Actor", "foo/actor.Customer"),
            ("System", "foo/system.Platform"),
            ("Container", "foo/container.Api"),
            ("Component", "foo/component.Gateway"),
        ] {
            let out = conn
                .query(&format!(
                    "MATCH (n:{label} {{fqn: '{fqn}'}}) RETURN n.name"
                ))
                .unwrap()
                .to_string();
            assert!(!out.contains("(empty)"), "{label} {fqn} missing: {out}");
        }
        // Kind-specific columns.
        let out = conn
            .query("MATCH (s:Subdomain {fqn: 'foo/subdomain.Access'}) RETURN s.kind")
            .unwrap()
            .to_string();
        assert!(out.contains("core"), "subdomain kind: {out}");
        let out = conn
            .query("MATCH (a:Aggregate {fqn: 'foo/aggregate.Session'}) RETURN a.root")
            .unwrap()
            .to_string();
        assert!(out.contains("Session"), "aggregate root: {out}");
        let out = conn
            .query("MATCH (c:Container {fqn: 'foo/container.Api'}) RETURN c.kind")
            .unwrap()
            .to_string();
        assert!(out.contains("app"), "container kind: {out}");

        // The spine resolves end to end: requirement → domain → system → code.
        let out = conn
            .query(
                "MATCH (r:Requirement)-[:Drives]->(d:Domain)-[:Realises]->(s:System)-[:Contains]->(:Container)-[:Contains]->(c:Component)-[:ImplementedBy]->(impl) RETURN impl.fqn",
            )
            .unwrap()
            .to_string();
        assert!(out.contains("mod.Gateway"), "spine to code: {out}");

        // The DDD hierarchy loads through the extended Contains table.
        let out = conn
            .query(
                "MATCH (d:Domain)-[:Contains]->(sd:Subdomain)-[:Contains]->(ag:Aggregate)-[:Contains]->(e:Entity) RETURN e.fqn",
            )
            .unwrap()
            .to_string();
        assert!(
            out.contains("foo/entity.User"),
            "domain hierarchy: {out}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
