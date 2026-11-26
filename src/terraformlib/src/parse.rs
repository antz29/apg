//! Terraform source parsing: HCL native syntax through hcl-edit, and JSON
//! syntax through serde_json, both mapped onto one uniform body view. Every
//! span is a decoded byte range into the text handed to `parse`.

use std::ops::Range;
use std::path::Path;

use hcl_edit::expr::{Expression, FuncCall, Traversal};
use hcl_edit::structure::{Body as HclBody, Structure};
use hcl_edit::template::ForTemplateExpr;
use hcl_edit::visit::{self, Visit};
use hcl_edit::Span;
use serde_json::{Map, Value};

/// The top-level blocks and attributes of one source file (or of a block body).
#[derive(Debug, Default)]
pub struct Body {
    pub blocks: Vec<Block>,
    pub attributes: Vec<Attribute>,
}

#[derive(Debug)]
pub struct Block {
    pub kind: String,
    pub labels: Vec<String>,
    pub body: Body,
    pub span: Range<usize>,
}

#[derive(Debug)]
pub struct Attribute {
    pub name: String,
    /// The value when it is a plain string with no template interpolation.
    pub literal: Option<String>,
    /// Traversal and function-call references, including those in `${…}`.
    pub refs: Vec<Reference>,
    pub span: Range<usize>,
}

#[derive(Debug)]
pub struct Reference {
    pub span: Range<usize>,
    pub target: Target,
}

#[derive(Debug)]
pub enum Target {
    Traversal(Traversal),
    Call(FuncCall),
}

/// A parse failure: 1-based line and column, and the parser's message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub line: usize,
    pub column: usize,
    pub message: String,
}

/// Parse `text` (decoded source of `path`): a `.json` file is JSON syntax,
/// anything else HCL native syntax.
pub fn parse(path: &Path, text: &str) -> Result<Body, ParseError> {
    if path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("json"))
    {
        parse_json(text)
    } else {
        parse_native(text)
    }
}

fn parse_native(text: &str) -> Result<Body, ParseError> {
    let body = hcl_edit::parser::parse_body(text).map_err(|e| {
        let loc = e.location();
        ParseError {
            line: loc.line(),
            column: loc.column(),
            message: e.message().to_string(),
        }
    })?;
    Ok(native_body(&body))
}

fn native_body(body: &HclBody) -> Body {
    let mut out = Body::default();
    for structure in body.iter() {
        match structure {
            Structure::Block(b) => out.blocks.push(Block {
                kind: b.ident.as_str().to_string(),
                labels: b.labels.iter().map(|l| l.as_str().to_string()).collect(),
                body: native_body(&b.body),
                span: b.span().unwrap_or(0..0),
            }),
            Structure::Attribute(a) => out.attributes.push(Attribute {
                name: a.key.as_str().to_string(),
                literal: a.value.as_str().map(str::to_string),
                refs: refs_of(&a.value),
                span: a.span().unwrap_or(0..0),
            }),
        }
    }
    out
}

fn refs_of(expr: &Expression) -> Vec<Reference> {
    let mut c = Collect::default();
    c.visit_expr(expr);
    c.0
}

/// Collects every traversal and function call, in source order per visit.
#[derive(Default)]
struct Collect(Vec<Reference>);

impl Visit for Collect {
    fn visit_traversal(&mut self, node: &Traversal) {
        self.0.push(Reference {
            span: node.span().unwrap_or(0..0),
            target: Target::Traversal(node.clone()),
        });
        visit::visit_traversal(self, node);
    }

    fn visit_func_call(&mut self, node: &FuncCall) {
        self.0.push(Reference {
            span: node.span().unwrap_or(0..0),
            target: Target::Call(node.clone()),
        });
        visit::visit_func_call(self, node);
    }

    // hcl-edit 0.10.0's default skips the `for` directive's collection expression.
    fn visit_for_template_expr(&mut self, node: &ForTemplateExpr) {
        self.visit_expr(&node.collection_expr);
        visit::visit_for_template_expr(self, node);
    }
}

fn parse_json(text: &str) -> Result<Body, ParseError> {
    let whole = 0..text.len();
    let root: Value = serde_json::from_str(text).map_err(|e| ParseError {
        line: e.line(),
        column: e.column(),
        message: e.to_string(),
    })?;
    let Value::Object(map) = root else {
        return Err(json_error("expected a JSON object at the root".into()));
    };
    json_root(&map, &whole)
}

/// Root properties: an object (or an array of objects) is a block of that
/// type, with one nested object level per label; anything else an attribute.
fn json_root(map: &Map<String, Value>, whole: &Range<usize>) -> Result<Body, ParseError> {
    let mut out = Body::default();
    for (key, value) in map.iter().filter(|(k, _)| !k.starts_with("//")) {
        let depth = label_count(key);
        match value {
            Value::Object(_) => {
                json_blocks(key, depth, value, &mut Vec::new(), whole, &mut out.blocks)?
            }
            Value::Array(items) if !items.is_empty() && items.iter().all(Value::is_object) => {
                for item in items {
                    json_blocks(key, depth, item, &mut Vec::new(), whole, &mut out.blocks)?;
                }
            }
            _ => out.attributes.push(json_attribute(key, value, whole)?),
        }
    }
    Ok(out)
}

fn json_blocks(
    kind: &str,
    depth: usize,
    value: &Value,
    labels: &mut Vec<String>,
    whole: &Range<usize>,
    out: &mut Vec<Block>,
) -> Result<(), ParseError> {
    let Value::Object(map) = value else {
        return Err(json_error(format!("block `{kind}` expects a JSON object")));
    };
    if depth == 0 {
        let attributes = map
            .iter()
            .filter(|(k, _)| !k.starts_with("//"))
            .map(|(k, v)| json_attribute(k, v, whole))
            .collect::<Result<_, _>>()?;
        out.push(Block {
            kind: kind.to_string(),
            labels: labels.clone(),
            body: Body {
                blocks: Vec::new(),
                attributes,
            },
            span: whole.clone(),
        });
        return Ok(());
    }
    for (label, sub) in map.iter().filter(|(k, _)| !k.starts_with("//")) {
        labels.push(label.clone());
        json_blocks(kind, depth - 1, sub, labels, whole, out)?;
        labels.pop();
    }
    Ok(())
}

fn json_attribute(
    name: &str,
    value: &Value,
    whole: &Range<usize>,
) -> Result<Attribute, ParseError> {
    let mut refs = Vec::new();
    json_refs(value, &mut refs)?;
    for r in &mut refs {
        r.span = whole.clone();
    }
    Ok(Attribute {
        name: name.to_string(),
        literal: value
            .as_str()
            .filter(|s| !s.contains("${") && !s.contains("%{"))
            .map(str::to_string),
        refs,
        span: whole.clone(),
    })
}

/// Every string under `value` is a template; its references are collected.
fn json_refs(value: &Value, out: &mut Vec<Reference>) -> Result<(), ParseError> {
    match value {
        Value::String(s) => {
            let template = hcl_edit::parser::parse_template(s)
                .map_err(|e| json_error(format!("template: {}", e.message())))?;
            let mut c = Collect::default();
            c.visit_template(&template);
            out.extend(c.0);
        }
        Value::Array(items) => {
            for item in items {
                json_refs(item, out)?;
            }
        }
        Value::Object(map) => {
            for (_, v) in map.iter().filter(|(k, _)| !k.starts_with("//")) {
                json_refs(v, out)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Labels a block type takes in Terraform/OpenTofu; an unknown type takes none.
fn label_count(kind: &str) -> usize {
    match kind {
        "resource" | "data" => 2,
        "variable" | "output" | "module" | "provider" | "check" | "run" => 1,
        _ => 0,
    }
}

/// JSON errors carry no position; the whole file is reported at 1:1.
fn json_error(message: String) -> ParseError {
    ParseError {
        line: 1,
        column: 1,
        message,
    }
}
