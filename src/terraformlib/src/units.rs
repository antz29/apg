//! Block scanner: turns one Terraform module's parsed files into Struct units
//! (name, containing file, span, parent, and the block or attribute it came
//! from), the override blocks the resolver merges, and the diagnostics. Pure
//! over parsed input: the caller parses files and writes the diagnostics.

use std::collections::BTreeMap;
use std::ops::Range;

use crate::module::Module;
use crate::parse::{Attribute, Block, Body};
use crate::source::{span, Decoded, Span};

/// One successfully parsed file: repo-relative path, decoded text (for spans), body.
pub struct Parsed {
    pub path: String,
    pub text: Decoded,
    pub body: Body,
}

/// The parsed piece a unit was read from.
pub enum Element<'a> {
    Block(&'a Block),
    Attribute(&'a Attribute),
    /// A whole file: a test file, or a `.tfvars` file whose keys are its attributes.
    File(&'a Body),
}

pub struct Unit<'a> {
    pub name: String,
    pub file: &'a str,
    pub span: Span,
    /// 1-based column of the span start, for diagnostics.
    pub column: usize,
    /// Index into `ModuleUnits::structs` of the enclosing unit (a run's test file).
    pub parent: Option<usize>,
    pub element: Element<'a>,
}

pub struct Diagnostic {
    pub file: String,
    pub line: usize,
    pub column: usize,
    pub message: String,
}

#[derive(Default)]
pub struct ModuleUnits<'a> {
    pub structs: Vec<Unit<'a>>,
    /// Override-file blocks matching a base declaration, keyed by its Struct name.
    pub overrides: Vec<Unit<'a>>,
    pub diagnostics: Vec<Diagnostic>,
}

/// Scans the module whose directory is `dir` ("" for the root) from its `parsed` files.
/// Files absent from `parsed` (failed parses) contribute nothing.
pub fn module_units<'a>(dir: &str, module: &Module, parsed: &'a [Parsed]) -> ModuleUnits<'a> {
    let by_path: BTreeMap<&str, &'a Parsed> = parsed.iter().map(|p| (p.path.as_str(), p)).collect();
    let files = |paths: &[String]| -> Vec<&'a Parsed> {
        paths
            .iter()
            .filter_map(|p| by_path.get(p.as_str()).copied())
            .collect()
    };

    let mut out = ModuleUnits::default();
    // Base declarations, first one per name kept; a repeat is a diagnostic.
    let mut first: BTreeMap<String, (&'a str, usize)> = BTreeMap::new();
    for p in files(&module.base) {
        for u in named(p) {
            match first.get(&u.name) {
                Some(&(f, l)) => out.diagnostics.push(diag(
                    &u,
                    format!(
                        "duplicate declaration of `{}`, first declared at {f}:{l}",
                        u.name
                    ),
                )),
                None => {
                    first.insert(u.name.clone(), (u.file, u.span.start_line));
                    out.structs.push(u);
                }
            }
        }
    }
    for p in files(&module.overrides) {
        for u in named(p) {
            if first.contains_key(&u.name) {
                out.overrides.push(u);
            } else {
                let msg = format!("override block `{}` matches no base declaration", u.name);
                out.diagnostics.push(diag(&u, msg));
            }
        }
    }
    for p in files(&module.tests) {
        test_file(dir, p, &mut out.structs);
    }
    for p in files(&module.tfvars) {
        let name = format!("tfvars.{}", leaf(&p.path));
        out.structs
            .push(unit(p, name, whole(p), Element::File(&p.body), None));
    }
    for p in files(&module.locks) {
        for b in p.body.blocks.iter().filter(|b| b.kind == "provider") {
            if let [name] = b.labels.as_slice() {
                let name = format!("lock.{name}");
                out.structs
                    .push(unit(p, name, b.span.clone(), Element::Block(b), None));
            }
        }
    }
    out
}

/// The Structs of a base or override file: its resource, data, variable, output,
/// module and provider blocks, and one per entry of each locals block.
fn named(p: &Parsed) -> Vec<Unit<'_>> {
    let mut out = Vec::new();
    for block in &p.body.blocks {
        let name = match (block.kind.as_str(), block.labels.as_slice()) {
            ("resource", [t, n]) => format!("{t}.{n}"),
            ("data", [t, n]) => format!("data.{t}.{n}"),
            ("variable", [n]) => format!("var.{n}"),
            ("output", [n]) => format!("output.{n}"),
            ("module", [n]) => format!("module.{n}"),
            ("provider", [n]) => match literal_alias(block) {
                Some(alias) => format!("provider.{n}.{alias}"),
                None => format!("provider.{n}"),
            },
            ("locals", _) => {
                for a in &block.body.attributes {
                    let name = format!("local.{}", a.name);
                    out.push(unit(p, name, a.span.clone(), Element::Attribute(a), None));
                }
                continue;
            }
            _ => continue,
        };
        out.push(unit(
            p,
            name,
            block.span.clone(),
            Element::Block(block),
            None,
        ));
    }
    out
}

fn literal_alias(block: &Block) -> Option<&str> {
    block
        .body
        .attributes
        .iter()
        .find(|a| a.name == "alias")
        .and_then(|a| a.literal.as_deref())
}

/// A test file is one Struct named by its path relative to its module's
/// directory; each `run` block is a Struct under it.
fn test_file<'a>(dir: &str, p: &'a Parsed, out: &mut Vec<Unit<'a>>) {
    let prefix = if dir.is_empty() {
        String::new()
    } else {
        format!("{dir}/")
    };
    let rel = p.path.strip_prefix(prefix.as_str()).unwrap_or(&p.path);
    out.push(unit(
        p,
        format!("test.{rel}"),
        whole(p),
        Element::File(&p.body),
        None,
    ));
    let parent = Some(out.len() - 1);
    for run in p.body.blocks.iter().filter(|b| b.kind == "run") {
        if let [name] = run.labels.as_slice() {
            let name = format!("run.{name}");
            out.push(unit(p, name, run.span.clone(), Element::Block(run), parent));
        }
    }
}

fn unit<'a>(
    p: &'a Parsed,
    name: String,
    range: Range<usize>,
    element: Element<'a>,
    parent: Option<usize>,
) -> Unit<'a> {
    Unit {
        name,
        file: p.path.as_str(),
        span: span(&p.text, range.start, range.end),
        column: column(&p.text.text, range.start),
        parent,
        element,
    }
}

fn diag(u: &Unit, message: String) -> Diagnostic {
    Diagnostic {
        file: u.file.to_string(),
        line: u.span.start_line,
        column: u.column,
        message,
    }
}

fn whole(p: &Parsed) -> Range<usize> {
    0..p.text.text.len()
}

fn leaf(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// 1-based column (in chars) of decoded byte offset `at`.
fn column(text: &str, at: usize) -> usize {
    let line_start = text
        .get(..at)
        .and_then(|h| h.rfind('\n'))
        .map_or(0, |i| i + 1);
    text.get(line_start..at)
        .map_or(1, |s| s.chars().count() + 1)
}
