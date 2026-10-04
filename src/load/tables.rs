//! The load tables: one PARQUET file per node table and per rel-table
//! `(from, to)` pair ([`build_load_files`]), the DB schema
//! ([`create_schema`]), and the `COPY FROM` loader ([`copy_from`]). Also the
//! pair/node label enumerations the schema, the loader, and the write-through
//! merge guard share.

use std::collections::HashMap;
use std::path::Path;

use lbug::Connection;
use parquet::file::reader::{FileReader, SerializedFileReader};

use crate::graph::{Graph, NodeKind};
use crate::layers::{NodeProperties, properties_json};

use super::parquet::{Col, lines, loc, write_parquet};

/// Writes one PARQUET file per node table and one per rel-table `(from, to)`
/// pair into `dir`. Columns match the LadybugDB table schema exactly.
pub fn build_load_files(graph: &Graph, dir: &Path) -> anyhow::Result<()> {
    // --- Node tables ---
    let mut language_fqn = Vec::new();
    let mut module_fqn = Vec::new();
    let mut module_status = Vec::new();
    let mut scan_fqn = Vec::new();
    let mut scan_git_sha = Vec::new();
    let mut scan_git_clean = Vec::new();
    let mut scan_content_key = Vec::new();
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
    let mut req_props = Vec::new();
    let mut note_fqn = Vec::new();
    let mut note_body = Vec::new();
    let mut note_kind = Vec::new();
    let mut note_props = Vec::new();
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
    let mut task_verb = Vec::new();
    let mut task_target = Vec::new();
    let mut task_new_fqn = Vec::new();

    // Tier-1/2/3 node tables (GraphModel-SPEC.md; PHASE_01).
    let mut stakeholder_fqn = Vec::new();
    let mut stakeholder_name = Vec::new();
    let mut stakeholder_body = Vec::new();
    let mut stakeholder_props = Vec::new();
    let mut entity_fqn = Vec::new();
    let mut entity_name = Vec::new();
    let mut entity_body = Vec::new();
    let mut entity_props = Vec::new();
    let mut system_fqn = Vec::new();
    let mut system_name = Vec::new();
    let mut system_body = Vec::new();
    let mut system_props = Vec::new();
    let mut container_fqn = Vec::new();
    let mut container_name = Vec::new();
    let mut container_kind = Vec::new();
    let mut container_body = Vec::new();
    let mut container_props = Vec::new();
    let mut component_fqn = Vec::new();
    let mut component_name = Vec::new();
    let mut component_body = Vec::new();
    let mut component_props = Vec::new();

    // New-model tier catalog (apg-projects SPEC §3.1).
    let mut user_fqn = Vec::new();
    let mut user_name = Vec::new();
    let mut user_body = Vec::new();
    let mut user_props = Vec::new();
    let mut group_fqn = Vec::new();
    let mut group_name = Vec::new();
    let mut group_attribute = Vec::new();
    let mut group_root = Vec::new();
    let mut group_body = Vec::new();
    let mut group_props = Vec::new();
    let mut value_fqn = Vec::new();
    let mut value_name = Vec::new();
    let mut value_body = Vec::new();
    let mut value_props = Vec::new();
    let mut service_fqn = Vec::new();
    let mut service_name = Vec::new();
    let mut service_body = Vec::new();
    let mut service_props = Vec::new();
    let mut person_fqn = Vec::new();
    let mut person_name = Vec::new();
    let mut person_body = Vec::new();
    let mut person_props = Vec::new();
    let mut constraint_fqn = Vec::new();
    let mut constraint_name = Vec::new();
    let mut constraint_body = Vec::new();
    let mut constraint_attaches_to = Vec::new();
    let mut constraint_props = Vec::new();

    for (fqn, node) in &graph.nodes {
        match node.kind {
            NodeKind::Language => {
                language_fqn.push(fqn.clone());
            }
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
                scan_content_key.push(node.content_key.clone().unwrap_or_default());
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
                req_props.push(properties_json(&node.properties));
            }
            NodeKind::Note => {
                note_fqn.push(fqn.clone());
                note_body.push(node.body.clone().unwrap_or_default());
                note_kind.push(node.sub_kind.clone().unwrap_or_default());
                note_props.push(properties_json(&node.properties));
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
                task_verb.push(node.verb.clone().unwrap_or_default());
                task_target.push(node.target.clone().unwrap_or_default());
                task_new_fqn.push(node.new_fqn.clone().unwrap_or_default());
            }
            NodeKind::Stakeholder => {
                stakeholder_fqn.push(fqn.clone());
                stakeholder_name.push(node.name.clone().unwrap_or_default());
                stakeholder_body.push(node.body.clone().unwrap_or_default());
                stakeholder_props.push(properties_json(&node.properties));
            }
            NodeKind::Entity => {
                entity_fqn.push(fqn.clone());
                entity_name.push(node.name.clone().unwrap_or_default());
                entity_body.push(node.body.clone().unwrap_or_default());
                entity_props.push(properties_json(&node.properties));
            }
            NodeKind::System => {
                system_fqn.push(fqn.clone());
                system_name.push(node.name.clone().unwrap_or_default());
                system_body.push(node.body.clone().unwrap_or_default());
                system_props.push(properties_json(&node.properties));
            }
            NodeKind::Container => {
                container_fqn.push(fqn.clone());
                container_name.push(node.name.clone().unwrap_or_default());
                container_kind.push(node.sub_kind.clone().unwrap_or_default());
                container_body.push(node.body.clone().unwrap_or_default());
                container_props.push(properties_json(&node.properties));
            }
            NodeKind::Component => {
                component_fqn.push(fqn.clone());
                component_name.push(node.name.clone().unwrap_or_default());
                component_body.push(node.body.clone().unwrap_or_default());
                component_props.push(properties_json(&node.properties));
            }
            NodeKind::User => {
                user_fqn.push(fqn.clone());
                user_name.push(node.name.clone().unwrap_or_default());
                user_body.push(node.body.clone().unwrap_or_default());
                user_props.push(properties_json(&node.properties));
            }
            NodeKind::Group => {
                group_fqn.push(fqn.clone());
                group_name.push(node.name.clone().unwrap_or_default());
                group_attribute.push(node.attribute.clone().unwrap_or_default());
                group_root.push(node.root.clone().unwrap_or_default());
                group_body.push(node.body.clone().unwrap_or_default());
                group_props.push(properties_json(&node.properties));
            }
            NodeKind::Value => {
                value_fqn.push(fqn.clone());
                value_name.push(node.name.clone().unwrap_or_default());
                value_body.push(node.body.clone().unwrap_or_default());
                value_props.push(properties_json(&node.properties));
            }
            NodeKind::Service => {
                service_fqn.push(fqn.clone());
                service_name.push(node.name.clone().unwrap_or_default());
                service_body.push(node.body.clone().unwrap_or_default());
                service_props.push(properties_json(&node.properties));
            }
            NodeKind::Person => {
                person_fqn.push(fqn.clone());
                person_name.push(node.name.clone().unwrap_or_default());
                person_body.push(node.body.clone().unwrap_or_default());
                person_props.push(properties_json(&node.properties));
            }
            NodeKind::Constraint => {
                constraint_fqn.push(fqn.clone());
                constraint_name.push(node.name.clone().unwrap_or_default());
                constraint_body.push(node.body.clone().unwrap_or_default());
                constraint_attaches_to.push(node.attaches_to.clone().unwrap_or_default());
                constraint_props.push(properties_json(&node.properties));
            }
        }
    }

    write_parquet(
        &dir.join("language.parquet"),
        &[("fqn", Col::Str(language_fqn))],
    )?;
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
            ("content_key", Col::Str(scan_content_key)),
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
            ("properties", Col::Str(req_props)),
        ],
    )?;
    write_parquet(
        &dir.join("note.parquet"),
        &[
            ("fqn", Col::Str(note_fqn)),
            ("body", Col::Str(note_body)),
            ("kind", Col::Str(note_kind)),
            ("properties", Col::Str(note_props)),
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
            ("verb", Col::Str(task_verb)),
            ("target", Col::Str(task_target)),
            ("new_fqn", Col::Str(task_new_fqn)),
        ],
    )?;
    write_parquet(
        &dir.join("stakeholder.parquet"),
        &[
            ("fqn", Col::Str(stakeholder_fqn)),
            ("name", Col::Str(stakeholder_name)),
            ("body", Col::Str(stakeholder_body)),
            ("properties", Col::Str(stakeholder_props)),
        ],
    )?;
    write_parquet(
        &dir.join("entity.parquet"),
        &[
            ("fqn", Col::Str(entity_fqn)),
            ("name", Col::Str(entity_name)),
            ("body", Col::Str(entity_body)),
            ("properties", Col::Str(entity_props)),
        ],
    )?;
    write_parquet(
        &dir.join("system.parquet"),
        &[
            ("fqn", Col::Str(system_fqn)),
            ("name", Col::Str(system_name)),
            ("body", Col::Str(system_body)),
            ("properties", Col::Str(system_props)),
        ],
    )?;
    write_parquet(
        &dir.join("container.parquet"),
        &[
            ("fqn", Col::Str(container_fqn)),
            ("name", Col::Str(container_name)),
            ("kind", Col::Str(container_kind)),
            ("body", Col::Str(container_body)),
            ("properties", Col::Str(container_props)),
        ],
    )?;
    write_parquet(
        &dir.join("component.parquet"),
        &[
            ("fqn", Col::Str(component_fqn)),
            ("name", Col::Str(component_name)),
            ("body", Col::Str(component_body)),
            ("properties", Col::Str(component_props)),
        ],
    )?;
    write_parquet(
        &dir.join("user.parquet"),
        &[
            ("fqn", Col::Str(user_fqn)),
            ("name", Col::Str(user_name)),
            ("body", Col::Str(user_body)),
            ("properties", Col::Str(user_props)),
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
            ("properties", Col::Str(group_props)),
        ],
    )?;
    write_parquet(
        &dir.join("value.parquet"),
        &[
            ("fqn", Col::Str(value_fqn)),
            ("name", Col::Str(value_name)),
            ("body", Col::Str(value_body)),
            ("properties", Col::Str(value_props)),
        ],
    )?;
    write_parquet(
        &dir.join("service.parquet"),
        &[
            ("fqn", Col::Str(service_fqn)),
            ("name", Col::Str(service_name)),
            ("body", Col::Str(service_body)),
            ("properties", Col::Str(service_props)),
        ],
    )?;
    write_parquet(
        &dir.join("person.parquet"),
        &[
            ("fqn", Col::Str(person_fqn)),
            ("name", Col::Str(person_name)),
            ("body", Col::Str(person_body)),
            ("properties", Col::Str(person_props)),
        ],
    )?;
    write_parquet(
        &dir.join("constraint.parquet"),
        &[
            ("fqn", Col::Str(constraint_fqn)),
            ("name", Col::Str(constraint_name)),
            ("body", Col::Str(constraint_body)),
            ("attaches_to", Col::Str(constraint_attaches_to)),
            ("properties", Col::Str(constraint_props)),
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

    // The node-file edge kind backing each durable rel table, so the load
    // files can look up the row's authored properties. `None` for the transient
    // plan/feedback rels, whose tables carry no serialized-properties column.
    let rel_kind = |table: &str| -> Option<&'static str> {
        match table {
            "Contains" => Some("contains"),
            "Calls" => Some("calls"),
            "Uses" => Some("uses"),
            "Details" => Some("details"),
            "DependsOn" => Some("depends-on"),
            "Drives" => Some("drives"),
            "Represents" => Some("represents"),
            "RealisedBy" => Some("realised-by"),
            "SpecImplementedBy" => Some("implemented-by"),
            "Publishes" => Some("publishes"),
            "Subscribes" => Some("subscribes"),
            _ => None,
        }
    };
    // A code-only/transient rel table's load file: the two endpoint columns.
    let rel_plain = |name: &str, from: Vec<String>, to: Vec<String>| -> anyhow::Result<()> {
        write_parquet(
            &dir.join(name),
            &[("from", Col::Str(from)), ("to", Col::Str(to))],
        )
    };
    // A durable authored rel table's load file: the endpoint columns plus the
    // serialized-properties column, looked up per row (an absent entry — a code
    // row or an authored edge with no properties — is the empty object, `"{}"`).
    let rel_props = |name: &str,
                     kind: &'static str,
                     from: Vec<String>,
                     to: Vec<String>|
     -> anyhow::Result<()> {
        let props: Vec<String> = from
            .iter()
            .zip(to.iter())
            .map(|(a, b)| {
                graph
                    .edge_properties
                    .get(&(kind, a.clone(), b.clone()))
                    .map(properties_json)
                    .unwrap_or_else(|| properties_json(&NodeProperties::new()))
            })
            .collect();
        write_parquet(
            &dir.join(name),
            &[
                ("from", Col::Str(from)),
                ("to", Col::Str(to)),
                ("properties", Col::Str(props)),
            ],
        )
    };
    rel_props("contains_mod_mod.parquet", "contains", c_mm.0, c_mm.1)?;
    rel_props(
        "contains_mod_file.parquet",
        "contains",
        c_mfile.0,
        c_mfile.1,
    )?;
    rel_props("contains_file_struct.parquet", "contains", c_fs.0, c_fs.1)?;
    rel_props("contains_file_fn.parquet", "contains", c_ff.0, c_ff.1)?;
    rel_props("contains_struct_struct.parquet", "contains", c_ss.0, c_ss.1)?;
    rel_props("contains_struct_fn.parquet", "contains", c_sf.0, c_sf.1)?;
    rel_props("calls_fn.parquet", "calls", calls_fn.0, calls_fn.1)?;
    rel_props("calls_svc.parquet", "calls", calls_svc.0, calls_svc.1)?;
    rel_props("uses_fn.parquet", "uses", u_fn.0, u_fn.1)?;
    rel_props("uses_struct.parquet", "uses", u_st.0, u_st.1)?;
    rel_props("uses_person.parquet", "uses", u_person.0, u_person.1)?;
    write_parquet(
        &dir.join("unresolved_call.parquet"),
        &[
            ("from", Col::Str(uc_from)),
            ("to", Col::Str(uc_to)),
            ("target_type", Col::Str(uc_tt)),
        ],
    )?;
    rel_plain("unresolved_use_fn.parquet", uu_fn.0, uu_fn.1)?;
    rel_plain("unresolved_use_struct.parquet", uu_st.0, uu_st.1)?;

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
        // The shared `Contains` table carries the serialized-properties column
        // for every pair, code rows included (they store the empty object).
        rel_props(&name, "contains", fa.clone(), fb.clone())?;
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
        let name = pair_file(table, from, to);
        // The durable authored rel tables carry the serialized-properties
        // column; the transient plan/feedback rels (`Reviews`/`Gates`/
        // `Satisfies`) have no column and take the plain two-column file.
        match rel_kind(table) {
            Some(kind) => rel_props(&name, kind, fa, fb)?,
            None => rel_plain(&name, fa, fb)?,
        }
    }

    Ok(())
}

/// The `(from, to)` kind pairs of the extended `Contains` table (SPEC §7, R2,
/// R21), with the per-pair PARQUET filename (internal to the load dir).
fn contains_pairs() -> Vec<(NodeKind, NodeKind)> {
    use NodeKind::*;
    vec![
        // The Language root ⊃ its modules (PHASE_09 language rooting).
        (Language, Module),
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
pub(crate) fn spec_rel_pairs() -> Vec<(&'static str, NodeKind, NodeKind)> {
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
    // `Note` is reviewable — a reviewer can attach Feedback to a note — but it
    // is deliberately NOT a `Details` target: `Note`→`Note` `Details` must stay
    // refused, so the pair is added to `Reviews` only, never to the shared
    // `Details` list above.
    v.push(("Reviews", Feedback, Note));
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
        NodeKind::Language => "language",
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

/// The rel-table pairs only the write-through merge guard needs: the authored
/// `calls` (Service→Service) and `uses` (Person→System) edges share the code
/// rel tables `Calls`/`Uses`, whose parquet files and COPY statements are
/// written by the explicit code-rel branches in `build_load_files`/`copy_from`
/// (`calls_svc.parquet`, `uses_person.parquet`). They must NOT join
/// [`spec_rel_pairs`]: that enumeration feeds `build_load_files` and
/// `copy_from`, so a second entry would double-write those files and emit
/// duplicate COPY statements.
const AUTHORED_REL_PAIRS: [(&str, &str, &str); 2] = [
    ("Calls", "Service", "Service"),
    ("Uses", "Person", "System"),
];

/// Every `(rel_table, from_label, to_label)` triple the DB schema declares —
/// derived from the same pair enumerations that write the load files
/// (`build_load_files`) and the `CREATE REL TABLE` statements
/// (`create_schema`), plus the authored-only pairs ([`AUTHORED_REL_PAIRS`]),
/// so the write-through merge guard in `artifacts.rs`
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
        v.extend(AUTHORED_REL_PAIRS);
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
        "Language",
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
        NodeKind::Language => "Language",
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
    conn.query("CREATE NODE TABLE Language(fqn STRING PRIMARY KEY)")?;
    conn.query(
        "CREATE NODE TABLE Scan(fqn STRING PRIMARY KEY, git_sha STRING, git_clean STRING, content_key STRING, scanned_at STRING)",
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
        "CREATE NODE TABLE Requirement(fqn STRING PRIMARY KEY, id STRING, title STRING, body STRING, feature STRING, properties STRING)",
    )?;
    conn.query(
        "CREATE NODE TABLE Note(fqn STRING PRIMARY KEY, body STRING, kind STRING, properties STRING)",
    )?;
    conn.query(
        "CREATE NODE TABLE Feedback(fqn STRING PRIMARY KEY, body STRING, status STRING, disposition STRING)",
    )?;
    conn.query("CREATE NODE TABLE Plan(fqn STRING PRIMARY KEY, title STRING, strategy STRING)")?;
    conn.query(
        "CREATE NODE TABLE PlanPhase(fqn STRING PRIMARY KEY, number INT64, title STRING, deliverable STRING, status STRING)",
    )?;
    conn.query(
        "CREATE NODE TABLE Task(fqn STRING PRIMARY KEY, title STRING, kind STRING, tier STRING, status STRING, verb STRING, target STRING, new_fqn STRING)",
    )?;
    conn.query(
        "CREATE NODE TABLE Stakeholder(fqn STRING PRIMARY KEY, name STRING, body STRING, properties STRING)",
    )?;
    conn.query(
        "CREATE NODE TABLE Entity(fqn STRING PRIMARY KEY, name STRING, body STRING, properties STRING)",
    )?;
    conn.query(
        "CREATE NODE TABLE System(fqn STRING PRIMARY KEY, name STRING, body STRING, properties STRING)",
    )?;
    conn.query(
        "CREATE NODE TABLE Container(fqn STRING PRIMARY KEY, name STRING, kind STRING, body STRING, properties STRING)",
    )?;
    conn.query(
        "CREATE NODE TABLE Component(fqn STRING PRIMARY KEY, name STRING, body STRING, properties STRING)",
    )?;
    conn.query(
        "CREATE NODE TABLE User(fqn STRING PRIMARY KEY, name STRING, body STRING, properties STRING)",
    )?;
    conn.query(
        "CREATE NODE TABLE DomainGroup(fqn STRING PRIMARY KEY, name STRING, attribute STRING, root STRING, body STRING, properties STRING)",
    )?;
    conn.query(
        "CREATE NODE TABLE Value(fqn STRING PRIMARY KEY, name STRING, body STRING, properties STRING)",
    )?;
    conn.query(
        "CREATE NODE TABLE Service(fqn STRING PRIMARY KEY, name STRING, body STRING, properties STRING)",
    )?;
    conn.query(
        "CREATE NODE TABLE Person(fqn STRING PRIMARY KEY, name STRING, body STRING, properties STRING)",
    )?;
    conn.query(
        "CREATE NODE TABLE Constraint(fqn STRING PRIMARY KEY, name STRING, body STRING, attaches_to STRING, properties STRING)",
    )?;
    // The durable authored rel tables gain the serialized-properties column.
    // `Contains`/`Calls`/`Uses` are shared with scanned code rows; those carry
    // the empty-object value (`"{}"`). The transient plan/feedback rels
    // (`Reviews`/`Gates`/`Satisfies`) and the code-only
    // `UnresolvedCall`/`UnresolvedUse` stay uncolumned.
    conn.query(
        "CREATE REL TABLE Contains(FROM Language TO Module, FROM Module TO Module, FROM Module TO File, FROM File TO Struct, FROM File TO Function, FROM Struct TO Struct, FROM Struct TO Function, FROM Plan TO PlanPhase, FROM PlanPhase TO Task, FROM Stakeholder TO Requirement, FROM User TO Requirement, FROM Requirement TO Requirement, FROM DomainGroup TO DomainGroup, FROM DomainGroup TO Entity, FROM DomainGroup TO Value, FROM DomainGroup TO Service, FROM System TO Container, FROM Container TO Component, properties STRING)",
    )?;
    conn.query(
        "CREATE REL TABLE Calls(FROM Function TO Function, FROM Service TO Service, properties STRING)",
    )?;
    conn.query(
        "CREATE REL TABLE Uses(FROM Function TO Struct, FROM Struct TO Struct, FROM Person TO System, properties STRING)",
    )?;
    conn.query(
        "CREATE REL TABLE UnresolvedCall(FROM Function TO UnresolvedTarget, target_type STRING)",
    )?;
    conn.query(
        "CREATE REL TABLE UnresolvedUse(FROM Function TO UnresolvedTarget, FROM Struct TO UnresolvedTarget)",
    )?;
    conn.query(
        "CREATE REL TABLE Details(FROM Note TO Module, FROM Note TO Function, FROM Note TO Struct, FROM Note TO File, FROM Note TO Requirement, FROM Note TO Plan, FROM Note TO PlanPhase, FROM Note TO Task, FROM Note TO Stakeholder, FROM Note TO Entity, FROM Note TO System, FROM Note TO Container, FROM Note TO Component, FROM Note TO User, FROM Note TO DomainGroup, FROM Note TO Value, FROM Note TO Service, FROM Note TO Person, FROM Note TO Constraint, properties STRING)",
    )?;
    conn.query(
        "CREATE REL TABLE Reviews(FROM Feedback TO Module, FROM Feedback TO Function, FROM Feedback TO Struct, FROM Feedback TO File, FROM Feedback TO Requirement, FROM Feedback TO Plan, FROM Feedback TO PlanPhase, FROM Feedback TO Task, FROM Feedback TO Stakeholder, FROM Feedback TO Entity, FROM Feedback TO System, FROM Feedback TO Container, FROM Feedback TO Component, FROM Feedback TO User, FROM Feedback TO DomainGroup, FROM Feedback TO Value, FROM Feedback TO Service, FROM Feedback TO Person, FROM Feedback TO Constraint, FROM Feedback TO Note)",
    )?;
    conn.query("CREATE REL TABLE DependsOn(FROM Requirement TO Requirement, properties STRING)")?;
    conn.query("CREATE REL TABLE Gates(FROM PlanPhase TO PlanPhase)")?;
    conn.query("CREATE REL TABLE Satisfies(FROM PlanPhase TO Requirement)")?;
    conn.query(
        "CREATE REL TABLE Drives(FROM Requirement TO DomainGroup, FROM Requirement TO Entity, FROM Requirement TO Value, FROM Requirement TO Service, properties STRING)",
    )?;
    conn.query(
        "CREATE REL TABLE Represents(FROM User TO Entity, FROM Entity TO Person, properties STRING)",
    )?;
    conn.query(
        "CREATE REL TABLE RealisedBy(FROM DomainGroup TO System, FROM DomainGroup TO Container, FROM DomainGroup TO Component, FROM Entity TO System, FROM Entity TO Container, FROM Entity TO Component, FROM Service TO System, FROM Service TO Container, FROM Service TO Component, properties STRING)",
    )?;
    conn.query(
        "CREATE REL TABLE SpecImplementedBy(FROM System TO Module, FROM System TO File, FROM System TO Struct, FROM System TO Function, FROM Container TO Module, FROM Container TO File, FROM Container TO Struct, FROM Container TO Function, FROM Component TO Module, FROM Component TO File, FROM Component TO Struct, FROM Component TO Function, properties STRING)",
    )?;
    conn.query("CREATE REL TABLE Publishes(FROM Service TO Entity, properties STRING)")?;
    conn.query("CREATE REL TABLE Subscribes(FROM Service TO Entity, properties STRING)")?;
    Ok(())
}

/// True when `path` exists and its parquet footer declares zero rows. A
/// missing or unreadable file returns `false` so the caller's `COPY` still
/// runs and fails loudly — a build/load drift must never be masked as "empty".
fn parquet_is_empty(path: &Path) -> bool {
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    match SerializedFileReader::new(file) {
        Ok(reader) => reader.metadata().file_metadata().num_rows() == 0,
        Err(_) => false,
    }
}

/// Loads all PARQUET files in `dir` via `COPY FROM`, per `(from, to)` pair for
/// multi-pair rel tables. A table whose load file is genuinely empty (zero
/// rows) is skipped: an empty `COPY` is pure per-statement overhead, and a
/// code-only scan leaves dozens of authored/spine tables empty.
pub fn copy_from(conn: &Connection, dir: &Path) -> anyhow::Result<()> {
    let p = |name: &str| dir.join(name).to_string_lossy().into_owned();
    let copy = |name: &str, stmt: String| -> anyhow::Result<()> {
        if parquet_is_empty(&dir.join(name)) {
            return Ok(());
        }
        conn.query(&stmt)?;
        Ok(())
    };
    let stmts: [(&str, String); 38] = [
        (
            "module.parquet",
            format!(r#"COPY Module FROM "{}""#, p("module.parquet")),
        ),
        (
            "language.parquet",
            format!(r#"COPY Language FROM "{}""#, p("language.parquet")),
        ),
        // The Scan row (SCAN_HEAD) carries the phase-01 content-identity key
        // (`content_key`) beside git_sha/git_clean/scanned_at; the parquet
        // columns are emitted in this exact DDL order, so the key lands in the
        // live DB's Scan node with no extra statement.
        (
            "scan.parquet",
            format!(r#"COPY Scan FROM "{}""#, p("scan.parquet")),
        ),
        (
            "struct.parquet",
            format!(r#"COPY Struct FROM "{}""#, p("struct.parquet")),
        ),
        (
            "function.parquet",
            format!(r#"COPY Function FROM "{}""#, p("function.parquet")),
        ),
        (
            "file.parquet",
            format!(r#"COPY File FROM "{}""#, p("file.parquet")),
        ),
        (
            "unresolved.parquet",
            format!(
                r#"COPY UnresolvedTarget FROM "{}""#,
                p("unresolved.parquet")
            ),
        ),
        (
            "requirement.parquet",
            format!(r#"COPY Requirement FROM "{}""#, p("requirement.parquet")),
        ),
        (
            "note.parquet",
            format!(r#"COPY Note FROM "{}""#, p("note.parquet")),
        ),
        (
            "feedback.parquet",
            format!(r#"COPY Feedback FROM "{}""#, p("feedback.parquet")),
        ),
        (
            "plan.parquet",
            format!(r#"COPY Plan FROM "{}""#, p("plan.parquet")),
        ),
        (
            "plan_phase.parquet",
            format!(r#"COPY PlanPhase FROM "{}""#, p("plan_phase.parquet")),
        ),
        (
            "task.parquet",
            format!(r#"COPY Task FROM "{}""#, p("task.parquet")),
        ),
        (
            "stakeholder.parquet",
            format!(r#"COPY Stakeholder FROM "{}""#, p("stakeholder.parquet")),
        ),
        (
            "entity.parquet",
            format!(r#"COPY Entity FROM "{}""#, p("entity.parquet")),
        ),
        (
            "system.parquet",
            format!(r#"COPY System FROM "{}""#, p("system.parquet")),
        ),
        (
            "container.parquet",
            format!(r#"COPY Container FROM "{}""#, p("container.parquet")),
        ),
        (
            "component.parquet",
            format!(r#"COPY Component FROM "{}""#, p("component.parquet")),
        ),
        (
            "user.parquet",
            format!(r#"COPY User FROM "{}""#, p("user.parquet")),
        ),
        (
            "group.parquet",
            format!(r#"COPY DomainGroup FROM "{}""#, p("group.parquet")),
        ),
        (
            "value.parquet",
            format!(r#"COPY Value FROM "{}""#, p("value.parquet")),
        ),
        (
            "service.parquet",
            format!(r#"COPY Service FROM "{}""#, p("service.parquet")),
        ),
        (
            "person.parquet",
            format!(r#"COPY Person FROM "{}""#, p("person.parquet")),
        ),
        (
            "constraint.parquet",
            format!(r#"COPY Constraint FROM "{}""#, p("constraint.parquet")),
        ),
        (
            "contains_mod_mod.parquet",
            format!(
                r#"COPY Contains FROM "{}" (from="Module", to="Module")"#,
                p("contains_mod_mod.parquet")
            ),
        ),
        (
            "contains_mod_file.parquet",
            format!(
                r#"COPY Contains FROM "{}" (from="Module", to="File")"#,
                p("contains_mod_file.parquet")
            ),
        ),
        (
            "contains_file_struct.parquet",
            format!(
                r#"COPY Contains FROM "{}" (from="File", to="Struct")"#,
                p("contains_file_struct.parquet")
            ),
        ),
        (
            "contains_file_fn.parquet",
            format!(
                r#"COPY Contains FROM "{}" (from="File", to="Function")"#,
                p("contains_file_fn.parquet")
            ),
        ),
        (
            "contains_struct_struct.parquet",
            format!(
                r#"COPY Contains FROM "{}" (from="Struct", to="Struct")"#,
                p("contains_struct_struct.parquet")
            ),
        ),
        (
            "contains_struct_fn.parquet",
            format!(
                r#"COPY Contains FROM "{}" (from="Struct", to="Function")"#,
                p("contains_struct_fn.parquet")
            ),
        ),
        (
            "calls_fn.parquet",
            format!(
                r#"COPY Calls FROM "{}" (from="Function", to="Function")"#,
                p("calls_fn.parquet")
            ),
        ),
        (
            "calls_svc.parquet",
            format!(
                r#"COPY Calls FROM "{}" (from="Service", to="Service")"#,
                p("calls_svc.parquet")
            ),
        ),
        (
            "uses_fn.parquet",
            format!(
                r#"COPY Uses FROM "{}" (from="Function", to="Struct")"#,
                p("uses_fn.parquet")
            ),
        ),
        (
            "uses_struct.parquet",
            format!(
                r#"COPY Uses FROM "{}" (from="Struct", to="Struct")"#,
                p("uses_struct.parquet")
            ),
        ),
        (
            "uses_person.parquet",
            format!(
                r#"COPY Uses FROM "{}" (from="Person", to="System")"#,
                p("uses_person.parquet")
            ),
        ),
        (
            "unresolved_call.parquet",
            format!(
                r#"COPY UnresolvedCall FROM "{}""#,
                p("unresolved_call.parquet")
            ),
        ),
        (
            "unresolved_use_fn.parquet",
            format!(
                r#"COPY UnresolvedUse FROM "{}" (from="Function", to="UnresolvedTarget")"#,
                p("unresolved_use_fn.parquet")
            ),
        ),
        (
            "unresolved_use_struct.parquet",
            format!(
                r#"COPY UnresolvedUse FROM "{}" (from="Struct", to="UnresolvedTarget")"#,
                p("unresolved_use_struct.parquet")
            ),
        ),
    ];
    for (name, s) in stmts {
        copy(name, s)?;
    }
    // Spec/plan rel tables: one COPY per `(from, to)` pair, generated from the
    // same pair enumeration that wrote the files.
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
        copy(
            &name,
            format!(
                r#"COPY Contains FROM "{}" (from="{}", to="{}")"#,
                p(&name),
                label_of(from),
                label_of(to)
            ),
        )?;
    }
    for (table, from, to) in spec_rel_pairs() {
        let name = pair_file(table, from, to);
        copy(
            &name,
            format!(
                r#"COPY {table} FROM "{}" (from="{}", to="{}")"#,
                p(&name),
                label_of(from),
                label_of(to)
            ),
        )?;
    }
    Ok(())
}
