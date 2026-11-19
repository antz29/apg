//! Per-file unit extraction: the `<script>` body plus every function, class,
//! enum, method and constructor declared outside the file's ERROR regions.

use std::collections::HashSet;

use tree_sitter::{Node, Tree};

use crate::source::{self, Decoded, Span};

/// What a unit declares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnitKind {
    ScriptBody,
    Function,
    Class,
    Enum,
    Method,
    Constructor,
}

/// One declaration unit of a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unit {
    pub kind: UnitKind,
    pub name: String,
    pub params: Vec<String>,
    pub overloaded: bool,
    /// Index of the owning Class unit (Method/Constructor only).
    pub owner: Option<usize>,
    pub span: Span,
    pub ordinal: usize,
    /// Written property type names, deduplicated in order (Class only).
    pub property_types: Vec<String>,
    /// Written base class/interface names, deduplicated in order (Class only).
    pub bases: Vec<String>,
}

/// A file's whole-file span and its units, `units[0]` the `<script>` body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileUnits {
    pub path: String,
    pub file_span: Span,
    pub units: Vec<Unit>,
}

/// Extracts the units of the file `path` decoded as `src` and parsed as `tree`.
pub fn scan_units(path: &str, src: &Decoded, tree: &Tree) -> FileUnits {
    // Whole file in raw bytes, BOM included; source::span would start past it.
    let file_span = Span {
        start: 0,
        end: src.raw_offset(src.text.len()),
        start_line: 1,
        end_line: source::line_count(&src.text),
    };
    let mut units = vec![unit(UnitKind::ScriptBody, "<script>", file_span, None)];
    let mut functions = HashSet::new();
    let mut types = HashSet::new();
    // Iterative pre-order walk: deep trees must not overflow the stack.
    let mut cursor = tree.walk();
    'walk: loop {
        let node = cursor.node();
        // Nothing inside an ERROR node (the root included) is a unit.
        let descend = !node.is_error();
        if descend {
            match node.kind() {
                "function_definition" => {
                    if let Some(name) = field_text(node, "name", src).map(bare_name) {
                        if functions.insert(name.to_lowercase()) {
                            units.push(unit(UnitKind::Function, name, span_of(src, node), None));
                        }
                    }
                }
                kind @ ("class_definition" | "enum_definition") => {
                    if let Some(name) = field_text(node, "name", src) {
                        if types.insert(name.to_lowercase()) {
                            let class = kind == "class_definition";
                            let k = if class {
                                UnitKind::Class
                            } else {
                                UnitKind::Enum
                            };
                            let mut u = unit(k, name, span_of(src, node), None);
                            if class {
                                u.bases = bases(node, src);
                            }
                            units.push(u);
                            if class {
                                members(node, units.len() - 1, src, &mut units);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        if descend && cursor.goto_first_child() {
            continue;
        }
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                break 'walk;
            }
        }
    }
    assign_ordinals(&mut units);
    FileUnits {
        path: path.to_string(),
        file_span,
        units,
    }
}

/// A class's written base class/interface names, `[]`-stripped and trimmed,
/// deduplicated case-insensitively in written order.
fn bases(class: Node<'_>, src: &Decoded) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut cur = class.walk();
    class
        .children_by_field_name("base_types", &mut cur)
        .filter(|b| b.kind() == "type_name")
        .map(|b| property_type_name(src.text[b.byte_range()].trim()).trim())
        .filter(|t| seen.insert(t.to_lowercase()))
        .map(str::to_string)
        .collect()
}

/// Pushes the Method/Constructor units of class unit `owner` and fills its
/// `property_types`.
fn members(class: Node<'_>, owner: usize, src: &Decoded, units: &mut Vec<Unit>) {
    let Some(body) = class.child_by_field_name("body") else {
        return;
    };
    let class_name = units[owner].name.to_lowercase();
    let first = units.len();
    let mut signatures = HashSet::new();
    let mut cur = body.walk();
    for m in body.children(&mut cur) {
        match m.kind() {
            "class_property" => {
                if let Some(t) = type_text(m, src).map(property_type_name) {
                    let props = &mut units[owner].property_types;
                    if !props.iter().any(|p| p == t) {
                        props.push(t.to_string());
                    }
                }
            }
            "class_method" => {
                let Some(name) = field_text(m, "name", src) else {
                    continue;
                };
                let (kind, name) = if name.to_lowercase() == class_name {
                    (UnitKind::Constructor, "new")
                } else {
                    (UnitKind::Method, name)
                };
                let mut params = Vec::new();
                if let Some(list) = m.child_by_field_name("parameters") {
                    let mut pc = list.walk();
                    params.extend(
                        list.children(&mut pc)
                            .filter(|p| p.kind() == "parameter")
                            .map(|p| param_type(type_text(p, src))),
                    );
                }
                let key = (
                    name.to_lowercase(),
                    params.iter().map(|p| p.to_lowercase()).collect::<Vec<_>>(),
                );
                if signatures.insert(key) {
                    let mut u = unit(kind, name, span_of(src, m), Some(owner));
                    u.params = params;
                    units.push(u);
                }
            }
            _ => {}
        }
    }
    // Same-named members form an overload group under its first spelling.
    for i in first..units.len() {
        let key = units[i].name.to_lowercase();
        let group: Vec<usize> = (first..units.len())
            .filter(|&j| units[j].name.to_lowercase() == key)
            .collect();
        if group.len() > 1 {
            let spelling = units[group[0]].name.clone();
            units[i].overloaded = true;
            units[i].name = spelling;
        }
    }
}

/// Numbers `units` 1-based by start offset, the ScriptBody first; ties keep
/// declaration order.
pub fn assign_ordinals(units: &mut [Unit]) {
    let mut order: Vec<usize> = (0..units.len()).collect();
    order.sort_by_key(|&i| (units[i].kind != UnitKind::ScriptBody, units[i].span.start));
    for (n, i) in order.into_iter().enumerate() {
        units[i].ordinal = n + 1;
    }
}

/// `name` without a leading `global:`/`script:`/`local:`/`private:` scope.
pub fn bare_name(name: &str) -> &str {
    ["global:", "script:", "local:", "private:"]
        .iter()
        .find(|p| {
            name.get(..p.len())
                .is_some_and(|h| h.eq_ignore_ascii_case(p))
        })
        .map_or(name, |p| &name[p.len()..])
}

/// A parameter's rendered type: its written type, or `object` when untyped.
pub fn param_type(written: Option<&str>) -> String {
    written.unwrap_or("object").to_string()
}

/// A property type name with its trailing `[]` array suffixes stripped.
pub fn property_type_name(written: &str) -> &str {
    let mut t = written;
    while let Some(s) = t.strip_suffix("[]") {
        t = s;
    }
    t
}

/// The type text written inside `node`'s `types` type_literal brackets.
fn type_text<'s>(node: Node<'_>, src: &'s Decoded) -> Option<&'s str> {
    let lit = node
        .child_by_field_name("types")
        .filter(|t| t.kind() == "type_literal")?;
    let text = &src.text[lit.byte_range()];
    Some(
        text.strip_prefix('[')
            .and_then(|t| t.strip_suffix(']'))
            .unwrap_or(text)
            .trim(),
    )
}

fn field_text<'s>(node: Node<'_>, field: &str, src: &'s Decoded) -> Option<&'s str> {
    node.child_by_field_name(field)
        .map(|n| &src.text[n.byte_range()])
}

fn span_of(src: &Decoded, node: Node<'_>) -> Span {
    source::span(src, node.start_byte(), node.end_byte())
}

fn unit(kind: UnitKind, name: &str, span: Span, owner: Option<usize>) -> Unit {
    Unit {
        kind,
        name: name.to_string(),
        params: Vec::new(),
        overloaded: false,
        owner,
        span,
        ordinal: 0,
        property_types: Vec::new(),
        bases: Vec::new(),
    }
}
