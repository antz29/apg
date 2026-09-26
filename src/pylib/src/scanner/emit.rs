//! Emission of module chains and resolved/unresolved call edges.

use std::io::Write;

use ty_ide::CallHierarchyItem;
use ty_project::ProjectDatabase;

use super::Scanner;
use crate::jsonl::write_record;
use crate::unresolved::{classify_unresolved, file_origin};

impl Scanner {
    /// Emits `module` records + a Module->Module `contains` chain for every
    /// dotted prefix of `identity` (deduplicated across files).
    pub(crate) fn emit_module_chain(&mut self, identity: &str, out: &mut impl Write) {
        let parts: Vec<&str> = identity.split('.').collect();
        let mut cur = String::new();
        for part in parts {
            let child = if cur.is_empty() {
                part.to_string()
            } else {
                format!("{cur}.{part}")
            };
            if self.seen_modules.insert(child.clone()) {
                write_record(out, &serde_json::json!({"type": "module", "fqn": child}));
            }
            if !cur.is_empty() {
                write_record(
                    out,
                    &serde_json::json!({"type": "contains", "from": cur, "to": child}),
                );
            }
            cur = child;
        }
    }

    pub(crate) fn emit_call_edge(
        &mut self,
        db: &ProjectDatabase,
        from_id: &str,
        to: &CallHierarchyItem,
        out: &mut impl Write,
    ) {
        // A resolved call whose target is a class (constructor invocation,
        // `Dog()`) is a `Uses` edge (Function->Struct), matching the schema
        // every other frontend follows; only a resolved function/method target
        // is a `Calls` edge (Function->Function).
        let edge_kind = if to.kind == ty_ide::SymbolKind::Class {
            "uses"
        } else {
            "calls"
        };
        let key = (to.file, u32::from(to.selection_range.start()));
        if let Some(target_id) = self.decl_by_pos.get(&key).cloned() {
            write_record(
                out,
                &serde_json::json!({
                    "type": edge_kind,
                    "from": from_id,
                    "to": self.endpoint(&target_id),
                }),
            );
            return;
        }
        let unresolved_fqn = match &to.detail {
            Some(module) if !module.is_empty() => format!("{module}.{}", to.name),
            _ => to.name.to_string(),
        };
        let (origin, path) = file_origin(db, to.file);
        let category = classify_unresolved(origin, &path, &self.root);
        self.emit_unresolved(&unresolved_fqn, category, out);
        if edge_kind == "uses" {
            write_record(
                out,
                &serde_json::json!({
                    "type": "unresolved_use",
                    "from": from_id,
                    "to": unresolved_fqn,
                }),
            );
        } else {
            write_record(
                out,
                &serde_json::json!({
                    "type": "unresolved_call",
                    "from": from_id,
                    "to": unresolved_fqn,
                    "target_type": "",
                }),
            );
        }
    }

    pub(crate) fn emit_unresolved(&mut self, fqn: &str, category: &str, out: &mut impl Write) {
        if fqn.is_empty() || !self.seen_unresolved.insert(fqn.to_string()) {
            return;
        }
        write_record(
            out,
            &serde_json::json!({"type": "unresolved", "fqn": fqn, "category": category}),
        );
    }
}
