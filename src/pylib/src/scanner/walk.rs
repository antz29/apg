//! The AST walkers: declaration collection (`walk_body`) and base-class
//! resolution (`walk_bases`).

use std::io::Write;

use ruff_db::files::File;
use ruff_python_ast::{self as ast, Stmt};
use ruff_text_size::{Ranged, TextSize};
use ty_ide::goto_definition;
use ty_project::{Db as _, ProjectDatabase};
use ty_python_core::ProgramFile;

use super::{Decl, DeclKind, Scanner};
use crate::jsonl::write_record;
use crate::unresolved::{classify_unresolved, file_origin, unresolved_name_for};

impl Scanner {
    /// Recursively walks a statement list, tracking the enclosing scope's
    /// FQN. Control-flow blocks (`if`/`for`/`while`/`with`/`try`/`match`)
    /// don't introduce a new Python scope, so their bodies are walked with
    /// the same `parent_fqn`; only `def`/`class` push a new scope. Declarations
    /// are always COLLECTED (the full resolution context) but only EMITTED
    /// when `emit` is true.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn walk_body(
        &mut self,
        body: &[Stmt],
        parent_fqn: &str,
        file: File,
        path: &str,
        li: &ruff_source_file::LineIndex,
        src: &str,
        emit: bool,
        out: &mut impl Write,
    ) {
        for stmt in body {
            match stmt {
                Stmt::FunctionDef(f) => {
                    let name = f.name.id.as_str();
                    let fqn = format!("{parent_fqn}.{name}");
                    let id = self.next_id();
                    let name_offset = f.name.range().start();
                    let (start, end) = (f.range().start(), f.range().end());
                    let start_line = li.line_index(start).get();
                    let end_line = li.line_index(end).get();
                    let params: Vec<String> = f
                        .parameters
                        .iter_non_variadic_params()
                        .filter(|p| !matches!(p.name().id.as_str(), "self" | "cls"))
                        .map(|p| {
                            p.annotation()
                                .map(|a| expr_source(a, src))
                                .unwrap_or_default()
                        })
                        .collect();
                    if emit {
                        write_record(
                            out,
                            &serde_json::json!({
                                "type": "function",
                                "id": id,
                                "parent": parent_fqn,
                                "name": name,
                                "params": params,
                                "file": path,
                                "path": path,
                                "start": u32::from(start),
                                "end": u32::from(end),
                                "start_line": start_line,
                                "end_line": end_line,
                            }),
                        );
                        if let Some(struct_id) = self.struct_id_by_fqn.get(parent_fqn).cloned() {
                            write_record(
                                out,
                                &serde_json::json!({
                                    "type": "contains",
                                    "from": self.endpoint(&struct_id),
                                    "to": id,
                                }),
                            );
                        }
                    }
                    self.decl_by_pos
                        .insert((file, u32::from(name_offset)), id.clone());
                    self.all_decls.push(Decl {
                        id,
                        kind: DeclKind::Function,
                        parent: parent_fqn.to_string(),
                        name: name.to_string(),
                        params,
                        canonical: String::new(),
                        file,
                        name_offset,
                        emit,
                    });
                    self.walk_body(&f.body, &fqn, file, path, li, src, emit, out);
                }
                Stmt::ClassDef(c) => {
                    let name = c.name.id.as_str();
                    let fqn = format!("{parent_fqn}.{name}");
                    let id = self.next_id();
                    let name_offset = c.name.range().start();
                    let (start, end) = (c.range().start(), c.range().end());
                    let start_line = li.line_index(start).get();
                    let end_line = li.line_index(end).get();
                    if emit {
                        write_record(
                            out,
                            &serde_json::json!({
                                "type": "struct",
                                "id": id,
                                "parent": parent_fqn,
                                "name": name,
                                "path": path,
                                "start": u32::from(start),
                                "end": u32::from(end),
                                "start_line": start_line,
                                "end_line": end_line,
                            }),
                        );
                        if let Some(struct_id) = self.struct_id_by_fqn.get(parent_fqn).cloned() {
                            write_record(
                                out,
                                &serde_json::json!({
                                    "type": "contains",
                                    "from": self.endpoint(&struct_id),
                                    "to": id,
                                }),
                            );
                        }
                    }
                    self.struct_id_by_fqn.insert(fqn.clone(), id.clone());
                    self.decl_by_pos
                        .insert((file, u32::from(name_offset)), id.clone());
                    self.all_decls.push(Decl {
                        id,
                        kind: DeclKind::Struct,
                        parent: parent_fqn.to_string(),
                        name: name.to_string(),
                        params: Vec::new(),
                        canonical: String::new(),
                        file,
                        name_offset,
                        emit,
                    });
                    self.walk_body(&c.body, &fqn, file, path, li, src, emit, out);
                }
                // Control-flow: same scope.
                Stmt::If(s) => {
                    self.walk_body(&s.body, parent_fqn, file, path, li, src, emit, out);
                    for clause in &s.elif_else_clauses {
                        self.walk_body(&clause.body, parent_fqn, file, path, li, src, emit, out);
                    }
                }
                Stmt::For(s) => {
                    self.walk_body(&s.body, parent_fqn, file, path, li, src, emit, out);
                    self.walk_body(&s.orelse, parent_fqn, file, path, li, src, emit, out);
                }
                Stmt::While(s) => {
                    self.walk_body(&s.body, parent_fqn, file, path, li, src, emit, out);
                    self.walk_body(&s.orelse, parent_fqn, file, path, li, src, emit, out);
                }
                Stmt::With(s) => {
                    self.walk_body(&s.body, parent_fqn, file, path, li, src, emit, out);
                }
                Stmt::Try(s) => {
                    self.walk_body(&s.body, parent_fqn, file, path, li, src, emit, out);
                    for handler in &s.handlers {
                        let ast::ExceptHandler::ExceptHandler(h) = handler;
                        self.walk_body(&h.body, parent_fqn, file, path, li, src, emit, out);
                    }
                    self.walk_body(&s.orelse, parent_fqn, file, path, li, src, emit, out);
                    self.walk_body(&s.finalbody, parent_fqn, file, path, li, src, emit, out);
                }
                Stmt::Match(s) => {
                    for case in &s.cases {
                        self.walk_body(&case.body, parent_fqn, file, path, li, src, emit, out);
                    }
                }
                _ => {}
            }
        }
    }

    /// Resolves each emitted class's base-class expressions to a `Uses` edge
    /// (or an `UnresolvedUse` when ty can't statically resolve it), walking
    /// the same scope structure as `walk_body` to recover each class's id.
    pub(crate) fn walk_bases(
        &mut self,
        db: &ProjectDatabase,
        body: &[Stmt],
        parent_fqn: &str,
        file: File,
        out: &mut impl Write,
    ) {
        for stmt in body {
            match stmt {
                Stmt::ClassDef(c) => {
                    let fqn = format!("{parent_fqn}.{}", c.name.id.as_str());
                    let class_id = self.struct_id_by_fqn.get(&fqn).cloned();
                    let emit_class = class_id
                        .as_ref()
                        .is_some_and(|id| self.emitted_id.contains(id));
                    if let (true, Some(class_id), Some(arguments)) =
                        (emit_class, class_id.clone(), c.arguments.as_deref())
                    {
                        for base in &arguments.args {
                            if let Some(name_offset) = base_identifier_offset(base) {
                                let program_file =
                                    ProgramFile::new(db, file, db.project().program(db));
                                if let Some(ranged) = goto_definition(db, program_file, name_offset)
                                {
                                    for target in &ranged.value {
                                        let key = (
                                            target.file(),
                                            u32::from(target.focus_range().start()),
                                        );
                                        let target_id = self.decl_by_pos.get(&key).cloned();
                                        if let Some(target_id) = target_id {
                                            write_record(
                                                out,
                                                &serde_json::json!({
                                                    "type": "uses",
                                                    "from": class_id,
                                                    "to": self.endpoint(&target_id),
                                                }),
                                            );
                                        } else {
                                            let unresolved_fqn =
                                                unresolved_name_for(db, target.file(), &self.root);
                                            let (origin, path) = file_origin(db, target.file());
                                            let category =
                                                classify_unresolved(origin, &path, &self.root);
                                            self.emit_unresolved(&unresolved_fqn, category, out);
                                            write_record(
                                                out,
                                                &serde_json::json!({
                                                    "type": "unresolved_use",
                                                    "from": class_id,
                                                    "to": unresolved_fqn,
                                                }),
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }
                    self.walk_bases(db, &c.body, &fqn, file, out);
                }
                Stmt::FunctionDef(f) => {
                    let fqn = format!("{parent_fqn}.{}", f.name.id.as_str());
                    self.walk_bases(db, &f.body, &fqn, file, out);
                }
                Stmt::If(s) => {
                    self.walk_bases(db, &s.body, parent_fqn, file, out);
                    for clause in &s.elif_else_clauses {
                        self.walk_bases(db, &clause.body, parent_fqn, file, out);
                    }
                }
                Stmt::For(s) => {
                    self.walk_bases(db, &s.body, parent_fqn, file, out);
                    self.walk_bases(db, &s.orelse, parent_fqn, file, out);
                }
                Stmt::While(s) => {
                    self.walk_bases(db, &s.body, parent_fqn, file, out);
                    self.walk_bases(db, &s.orelse, parent_fqn, file, out);
                }
                Stmt::With(s) => {
                    self.walk_bases(db, &s.body, parent_fqn, file, out);
                }
                Stmt::Try(s) => {
                    self.walk_bases(db, &s.body, parent_fqn, file, out);
                    for handler in &s.handlers {
                        let ast::ExceptHandler::ExceptHandler(h) = handler;
                        self.walk_bases(db, &h.body, parent_fqn, file, out);
                    }
                    self.walk_bases(db, &s.orelse, parent_fqn, file, out);
                    self.walk_bases(db, &s.finalbody, parent_fqn, file, out);
                }
                Stmt::Match(s) => {
                    for case in &s.cases {
                        self.walk_bases(db, &case.body, parent_fqn, file, out);
                    }
                }
                _ => {}
            }
        }
    }
}

/// The identifier offset naming a base-class expression (`Base` in
/// `class Foo(Base):`, or `Base` in `class Foo(module.Base):`), used as the
/// query point for `goto_definition`.
fn base_identifier_offset(expr: &ast::Expr) -> Option<TextSize> {
    match expr {
        ast::Expr::Name(n) => Some(n.range().start()),
        ast::Expr::Attribute(a) => Some(a.attr.range().start()),
        _ => None,
    }
}

/// Best-effort source snippet for a parameter annotation expression, used as
/// the erased "type" string in `params` (mirrors the other frontends' erased
/// parameter types, used ingestor-side only for overload-group
/// disambiguation).
fn expr_source(expr: &ast::Expr, src: &str) -> String {
    let range = expr.range();
    src.get(usize::from(range.start())..usize::from(range.end()))
        .unwrap_or("")
        .to_string()
}
