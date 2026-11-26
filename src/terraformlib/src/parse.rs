//! Terraform source parsing: HCL native syntax through hcl-edit, and JSON
//! syntax through a small position-tracking reader, both mapped onto one
//! uniform body view. Every span is a byte range into the text handed to
//! `parse`; a JSON `${…}` reference maps back to its raw source bytes, escapes
//! included.

use std::ops::Range;
use std::path::Path;

use hcl_edit::expr::{Expression, ForExpr, FuncCall, Traversal};
use hcl_edit::structure::{Body as HclBody, Structure};
use hcl_edit::template::ForTemplateExpr;
use hcl_edit::visit::{self, Visit};
use hcl_edit::Span;

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
    /// The bare identifier when the value is one (`provider = aws`), native syntax only.
    pub variable: Option<String>,
    /// Traversal and function-call references, including those in `${…}`.
    pub refs: Vec<Reference>,
    pub span: Range<usize>,
    /// The value expression's span (its start locates a non-literal source).
    pub value: Range<usize>,
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

/// Parse `text` (decoded source of `path`): a `.json` file is JSON syntax
/// (a `.tfvars.json` one binds its root properties as attributes), anything
/// else HCL native syntax.
pub fn parse(path: &Path, text: &str) -> Result<Body, ParseError> {
    if !path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("json"))
    {
        return parse_native(text);
    }
    let tfvars = path
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.to_ascii_lowercase().ends_with(".tfvars.json"));
    parse_json(text, tfvars)
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
                variable: a.value.as_variable().map(|v| v.as_str().to_string()),
                refs: refs_of(&a.value),
                span: a.span().unwrap_or(0..0),
                value: a.value.span().unwrap_or(0..0),
            }),
        }
    }
    out
}

fn refs_of(expr: &Expression) -> Vec<Reference> {
    let mut c = Collect::default();
    c.visit_expr(expr);
    c.refs
}

/// Collects every traversal and function call, in source order per visit. A
/// traversal rooted at a `for` iterator in scope is not a reference.
#[derive(Default)]
struct Collect {
    refs: Vec<Reference>,
    bound: Vec<String>,
}

impl Visit for Collect {
    fn visit_traversal(&mut self, node: &Traversal) {
        if let Expression::Variable(root) = &node.expr {
            if self.bound.iter().any(|b| b == root.as_str()) {
                for op in &node.operators {
                    self.visit_traversal_operator(op);
                }
                return;
            }
        }
        self.refs.push(Reference {
            span: node.span().unwrap_or(0..0),
            target: Target::Traversal(node.clone()),
        });
        visit::visit_traversal(self, node);
    }

    fn visit_func_call(&mut self, node: &FuncCall) {
        self.refs.push(Reference {
            span: node.span().unwrap_or(0..0),
            target: Target::Call(node.clone()),
        });
        visit::visit_func_call(self, node);
    }

    // hcl-edit 0.10.0's default skips the `for` expression's collection expression
    // and binds nothing; both are walked here with the iterators in scope.
    fn visit_for_expr(&mut self, node: &ForExpr) {
        self.visit_expr(&node.intro.collection_expr);
        let depth = self.bound.len();
        self.bound.extend(
            node.intro
                .key_var
                .iter()
                .chain([&node.intro.value_var])
                .map(|v| v.as_str().to_string()),
        );
        if let Some(k) = &node.key_expr {
            self.visit_expr(k);
        }
        self.visit_expr(&node.value_expr);
        if let Some(c) = &node.cond {
            self.visit_expr(&c.expr);
        }
        self.bound.truncate(depth);
    }

    // hcl-edit 0.10.0's default skips the `for` directive's collection expression.
    fn visit_for_template_expr(&mut self, node: &ForTemplateExpr) {
        self.visit_expr(&node.collection_expr);
        let depth = self.bound.len();
        self.bound.extend(
            node.key_var
                .iter()
                .chain([&node.value_var])
                .map(|v| v.as_str().to_string()),
        );
        self.visit_template(&node.template);
        self.bound.truncate(depth);
    }
}

/// A JSON value with the raw byte span of its token.
struct Node {
    span: Range<usize>,
    kind: Kind,
}

enum Kind {
    Object(Vec<Prop>),
    Array(Vec<Node>),
    Str(Str),
    Other,
}

/// One `"key": value` member, in source order; `span` runs from the key's
/// quote to the end of the value.
struct Prop {
    key: String,
    span: Range<usize>,
    value: Node,
}

/// A decoded string, and per decoded byte the raw source range it came from
/// (an escape sequence maps all its bytes to the escape's raw range).
struct Str {
    value: String,
    map: Vec<Range<usize>>,
}

impl Str {
    fn push(&mut self, c: char, raw: Range<usize>) {
        for _ in 0..c.len_utf8() {
            self.map.push(raw.clone());
        }
        self.value.push(c);
    }
}

fn parse_json(text: &str, tfvars: bool) -> Result<Body, ParseError> {
    let root = json_document(text)?;
    let Kind::Object(props) = &root.kind else {
        return Err(error_at(
            text,
            root.span.start,
            "expected a JSON object at the root".into(),
        ));
    };
    if tfvars {
        // Every root property of a .tfvars.json is an attribute, as in a native .tfvars.
        let attributes = props
            .iter()
            .filter(|p| !is_comment(&p.key))
            .map(|p| json_attribute(text, p))
            .collect::<Result<Vec<_>, _>>()?;
        return Ok(Body {
            blocks: Vec::new(),
            attributes,
        });
    }
    json_root(text, props)
}

/// Root properties: an object (or an array of objects) is a block of that
/// type, with one nested object level per label; anything else an attribute.
fn json_root(src: &str, props: &[Prop]) -> Result<Body, ParseError> {
    let mut out = Body::default();
    for p in props.iter().filter(|p| !is_comment(&p.key)) {
        let depth = label_count(&p.key);
        match &p.value.kind {
            Kind::Object(_) => json_blocks(
                src,
                &p.key,
                depth,
                &p.value,
                p.span.clone(),
                &mut Vec::new(),
                &mut out.blocks,
            )?,
            Kind::Array(items)
                if !items.is_empty() && items.iter().all(|i| matches!(i.kind, Kind::Object(_))) =>
            {
                for item in items {
                    json_blocks(
                        src,
                        &p.key,
                        depth,
                        item,
                        item.span.clone(),
                        &mut Vec::new(),
                        &mut out.blocks,
                    )?;
                }
            }
            _ => out.attributes.push(json_attribute(src, p)?),
        }
    }
    Ok(out)
}

/// Builds the block(s) for one object at `depth` remaining label levels. The
/// depth-0 arm maps each body property through `json_attribute`: a nested
/// object inside a block body is an attribute, not a nested block (no schema).
fn json_blocks(
    src: &str,
    kind: &str,
    depth: usize,
    node: &Node,
    span: Range<usize>,
    labels: &mut Vec<String>,
    out: &mut Vec<Block>,
) -> Result<(), ParseError> {
    let Kind::Object(props) = &node.kind else {
        return Err(error_at(
            src,
            node.span.start,
            format!("block `{kind}` expects a JSON object"),
        ));
    };
    if depth == 0 {
        let attributes = props
            .iter()
            .filter(|p| !is_comment(&p.key))
            .map(|p| json_attribute(src, p))
            .collect::<Result<Vec<_>, _>>()?;
        out.push(Block {
            kind: kind.to_string(),
            labels: labels.clone(),
            body: Body {
                blocks: Vec::new(),
                attributes,
            },
            span,
        });
        return Ok(());
    }
    for p in props.iter().filter(|p| !is_comment(&p.key)) {
        labels.push(p.key.clone());
        json_blocks(src, kind, depth - 1, &p.value, p.span.clone(), labels, out)?;
        labels.pop();
    }
    Ok(())
}

fn json_attribute(src: &str, p: &Prop) -> Result<Attribute, ParseError> {
    let mut refs = Vec::new();
    json_refs(src, &p.value, &mut refs)?;
    Ok(Attribute {
        name: p.key.clone(),
        literal: match &p.value.kind {
            Kind::Str(s) if !s.value.contains("${") && !s.value.contains("%{") => {
                Some(s.value.clone())
            }
            _ => None,
        },
        variable: None,
        refs,
        span: p.span.clone(),
        value: p.value.span.clone(),
    })
}

/// Every string under `node` is a template; its references are collected and
/// mapped back to the raw bytes of the string.
fn json_refs(src: &str, node: &Node, out: &mut Vec<Reference>) -> Result<(), ParseError> {
    match &node.kind {
        Kind::Str(s) => {
            let template = hcl_edit::parser::parse_template(&s.value).map_err(|e| {
                let raw = s
                    .map
                    .get(e.location().offset())
                    .map_or(node.span.end, |r| r.start);
                error_at(src, raw, format!("template: {}", e.message()))
            })?;
            let mut c = Collect::default();
            c.visit_template(&template);
            out.extend(c.refs.into_iter().map(|r| Reference {
                span: raw_span(&s.map, &node.span, r.span),
                target: r.target,
            }));
        }
        Kind::Array(items) => {
            for item in items {
                json_refs(src, item, out)?;
            }
        }
        Kind::Object(props) => {
            for p in props.iter().filter(|p| !is_comment(&p.key)) {
                json_refs(src, &p.value, out)?;
            }
        }
        Kind::Other => {}
    }
    Ok(())
}

/// The raw span of decoded template range `r`; an unspanned reference falls
/// back to the whole string token.
fn raw_span(map: &[Range<usize>], whole: &Range<usize>, r: Range<usize>) -> Range<usize> {
    match (r.start < r.end).then(|| (map.get(r.start), map.get(r.end - 1))) {
        Some((Some(s), Some(e))) => s.start..e.end,
        _ => whole.clone(),
    }
}

fn is_comment(key: &str) -> bool {
    key.starts_with("//")
}

/// Labels a block type takes in Terraform/OpenTofu; an unknown type takes none.
fn label_count(kind: &str) -> usize {
    match kind {
        "resource" | "data" => 2,
        "variable" | "output" | "module" | "provider" | "check" | "run" => 1,
        _ => 0,
    }
}

/// 1-based line and column (column counted in chars) of byte offset `at`.
fn error_at(src: &str, at: usize, message: String) -> ParseError {
    let head = &src.as_bytes()[..at.min(src.len())];
    let line_start = head.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
    ParseError {
        line: head.iter().filter(|&&b| b == b'\n').count() + 1,
        column: String::from_utf8_lossy(&head[line_start..]).chars().count() + 1,
        message,
    }
}

fn json_document(src: &str) -> Result<Node, ParseError> {
    let mut r = Reader { src, pos: 0 };
    r.skip_ws();
    let root = r.value()?;
    r.skip_ws();
    if r.pos != src.len() {
        return r.fail("trailing characters after the root value");
    }
    Ok(root)
}

/// A strict JSON reader that tracks byte positions. It accepts exactly the
/// RFC 8259 grammar, so each error points at the byte that broke it.
struct Reader<'a> {
    src: &'a str,
    pos: usize,
}

impl Reader<'_> {
    fn peek(&self) -> Option<u8> {
        self.src.as_bytes().get(self.pos).copied()
    }

    fn char_here(&self) -> Option<char> {
        self.src.get(self.pos..)?.chars().next()
    }

    fn fail<T>(&self, message: &str) -> Result<T, ParseError> {
        Err(error_at(self.src, self.pos, message.to_string()))
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn value(&mut self) -> Result<Node, ParseError> {
        let start = self.pos;
        let kind = match self.peek() {
            Some(b'{') => Kind::Object(self.object()?),
            Some(b'[') => Kind::Array(self.array()?),
            Some(b'"') => Kind::Str(self.string()?),
            Some(b'-' | b'0'..=b'9') => self.number()?,
            _ => {
                let word = ["true", "false", "null"]
                    .into_iter()
                    .find(|w| self.src.get(self.pos..).is_some_and(|s| s.starts_with(*w)));
                let Some(word) = word else {
                    return self.fail("expected a JSON value");
                };
                self.pos += word.len();
                Kind::Other
            }
        };
        Ok(Node {
            span: start..self.pos,
            kind,
        })
    }

    fn object(&mut self) -> Result<Vec<Prop>, ParseError> {
        self.pos += 1;
        let mut props = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(props);
        }
        loop {
            self.skip_ws();
            if self.peek() != Some(b'"') {
                return self.fail("expected a string key");
            }
            let key_start = self.pos;
            let key = self.string()?;
            self.skip_ws();
            if self.peek() != Some(b':') {
                return self.fail("expected `:` after key");
            }
            self.pos += 1;
            self.skip_ws();
            let value = self.value()?;
            let span = key_start..value.span.end;
            props.push(Prop {
                key: key.value,
                span,
                value,
            });
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(props);
                }
                _ => return self.fail("expected `,` or `}` in object"),
            }
        }
    }

    fn array(&mut self) -> Result<Vec<Node>, ParseError> {
        self.pos += 1;
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(items);
        }
        loop {
            self.skip_ws();
            items.push(self.value()?);
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b']') => {
                    self.pos += 1;
                    return Ok(items);
                }
                _ => return self.fail("expected `,` or `]` in array"),
            }
        }
    }

    fn string(&mut self) -> Result<Str, ParseError> {
        self.pos += 1;
        let mut out = Str {
            value: String::new(),
            map: Vec::new(),
        };
        loop {
            let at = self.pos;
            let Some(c) = self.char_here() else {
                return self.fail("unterminated string");
            };
            self.pos += c.len_utf8();
            match c {
                '"' => return Ok(out),
                '\\' => {
                    let e = self.escape(at)?;
                    out.push(e, at..self.pos);
                }
                c if u32::from(c) < 0x20 => {
                    return Err(error_at(self.src, at, "control character in string".into()));
                }
                c => out.push(c, at..self.pos),
            }
        }
    }

    /// The character an escape (after its backslash, which starts at `at`) denotes.
    fn escape(&mut self, at: usize) -> Result<char, ParseError> {
        let Some(c) = self.char_here() else {
            return self.fail("unterminated escape");
        };
        self.pos += c.len_utf8();
        Ok(match c {
            '"' | '\\' | '/' => c,
            'b' => '\u{8}',
            'f' => '\u{c}',
            'n' => '\n',
            'r' => '\r',
            't' => '\t',
            'u' => {
                let hi = self.hex4()?;
                let code = if (0xD800..0xDC00).contains(&hi) {
                    if self.src.get(self.pos..self.pos + 2) != Some("\\u") {
                        return self.fail("unpaired surrogate in escape");
                    }
                    self.pos += 2;
                    let lo = self.hex4()?;
                    if !(0xDC00..0xE000).contains(&lo) {
                        return self.fail("unpaired surrogate in escape");
                    }
                    0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
                } else {
                    hi
                };
                match char::from_u32(code) {
                    Some(c) => c,
                    None => return self.fail("invalid unicode escape"),
                }
            }
            _ => return Err(error_at(self.src, at, "invalid escape in string".into())),
        })
    }

    fn hex4(&mut self) -> Result<u32, ParseError> {
        let digits = self
            .src
            .get(self.pos..self.pos + 4)
            .filter(|d| d.bytes().all(|b| b.is_ascii_hexdigit()));
        let Some(digits) = digits else {
            return self.fail("invalid unicode escape");
        };
        self.pos += 4;
        Ok(u32::from_str_radix(digits, 16).unwrap_or_default())
    }

    fn number(&mut self) -> Result<Kind, ParseError> {
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        match self.peek() {
            Some(b'0') => self.pos += 1,
            Some(b'1'..=b'9') => {
                self.digits();
            }
            _ => return self.fail("invalid number"),
        }
        if self.peek() == Some(b'.') {
            self.pos += 1;
            if self.digits() == 0 {
                return self.fail("expected digits after `.`");
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.pos += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            if self.digits() == 0 {
                return self.fail("expected digits in exponent");
            }
        }
        Ok(Kind::Other)
    }

    fn digits(&mut self) -> usize {
        let start = self.pos;
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.pos += 1;
        }
        self.pos - start
    }
}
