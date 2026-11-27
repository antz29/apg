//! Reference resolver: turns one module's Struct units into the uses, module-source
//! and unresolved facts the scan emits. Pure over parsed input: the caller passes
//! every module (a local module source resolves across modules) and writes the records.

use std::collections::{BTreeMap, HashMap, HashSet};

use hcl_edit::expr::{Expression, Traversal, TraversalOperator};

use crate::module::Module;
use crate::parse::{Attribute, Block, Body, Reference, Target};
use crate::units::{Element, ModuleUnits, Parsed, Unit};

/// One resolved fact, keyed by indices into `ModuleUnits::structs`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Fact {
    /// A Struct-to-Struct `uses` edge.
    Uses { from: usize, to: usize },
    /// A Struct-to-File `uses` edge: kind `module-source` or `override`; `path` is repo-relative.
    FileUses {
        from: usize,
        path: String,
        kind: &'static str,
    },
    /// A target outside the module's declarations, or a non-local module source.
    Unresolved {
        from: usize,
        name: String,
        category: &'static str,
    },
}

/// Evaluation-context roots that draw no edge.
const CONTEXT: [&str; 5] = ["count", "each", "self", "path", "terraform"];

/// Resolves the facts of the module at `dir` (its units from `module_units`).
/// Files absent from `parsed` contribute nothing; a `dir` absent from `modules` yields none.
pub fn resolve(
    dir: &str,
    modules: &BTreeMap<String, Module>,
    units: &ModuleUnits<'_>,
    parsed: &[Parsed],
) -> Vec<Fact> {
    let Some(module) = modules.get(dir) else {
        return Vec::new();
    };
    let base: HashSet<&str> = module.base.iter().map(String::as_str).collect();
    let mut declared = HashMap::new();
    for (i, u) in units.structs.iter().enumerate() {
        if base.contains(u.file) {
            declared.entry(u.name.clone()).or_insert(i);
        }
    }
    let mut r = Resolver {
        dir,
        modules,
        parsed,
        declared,
        file: String::new(),
        bound: Vec::new(),
        facts: Vec::new(),
        seen: HashSet::new(),
    };
    for (i, u) in units.structs.iter().enumerate() {
        r.unit(i, u);
    }
    // An override block's references belong to the base declaration it overrides.
    for o in &units.overrides {
        let from = match r.declared.get(&o.name) {
            Some(&i) => i,
            None => continue,
        };
        r.fact(Fact::FileUses {
            from,
            path: o.file.to_string(),
            kind: "override",
        });
        r.unit(from, o);
    }
    r.facts
}

struct Resolver<'a> {
    dir: &'a str,
    modules: &'a BTreeMap<String, Module>,
    parsed: &'a [Parsed],
    /// Base declaration name to its Struct index.
    declared: HashMap<String, usize>,
    /// The repo-relative file of the unit being walked.
    file: String,
    /// `dynamic` block iterator names in scope.
    bound: Vec<String>,
    facts: Vec<Fact>,
    seen: HashSet<Fact>,
}

impl Resolver<'_> {
    fn fact(&mut self, f: Fact) {
        if self.seen.insert(f.clone()) {
            self.facts.push(f);
        }
    }

    fn unit(&mut self, from: usize, u: &Unit) {
        self.file = u.file.to_string();
        match u.element {
            Element::Block(b) => self.block(from, b),
            Element::Attribute(a) => self.attr(from, "", a),
            Element::File(body) if u.name.starts_with("tfvars.") => {
                for a in &body.attributes {
                    self.target(from, format!("var.{}", a.name));
                }
            }
            // A test file: its own top-level blocks, with each `run` walked as its own unit.
            Element::File(body) => {
                for a in &body.attributes {
                    self.attr(from, "", a);
                }
                for b in body.blocks.iter().filter(|b| b.kind != "run") {
                    self.block(from, b);
                }
            }
        }
    }

    fn block(&mut self, from: usize, b: &Block) {
        if b.kind != "dynamic" {
            return self.body(from, &b.body, b.kind == "module");
        }
        let iterator = b
            .body
            .attributes
            .iter()
            .find(|a| a.name == "iterator")
            .and_then(|a| a.variable.as_deref().or(a.literal.as_deref()));
        let name = iterator.or(b.labels.first().map(String::as_str));
        self.bound.push(name.unwrap_or_default().to_string());
        self.body(from, &b.body, false);
        self.bound.pop();
    }

    fn body(&mut self, from: usize, body: &Body, in_module: bool) {
        for a in &body.attributes {
            if in_module && a.name == "source" {
                self.module_source(from, a);
            } else {
                self.attr(from, &a.name, a);
            }
        }
        for b in &body.blocks {
            self.block(from, b);
        }
    }

    fn attr(&mut self, from: usize, name: &str, a: &Attribute) {
        if name == "provider" {
            if let Some(v) = &a.variable {
                self.target(from, format!("provider.{v}"));
            }
        }
        for r in &a.refs {
            self.reference(from, name, r);
        }
    }

    fn reference(&mut self, from: usize, attr: &str, r: &Reference) {
        let Target::Traversal(t) = &r.target else {
            return;
        };
        let Some(parts) = parts(t) else {
            return;
        };
        if self.bound.iter().any(|b| b == parts[0]) {
            return;
        }
        let key = match attr {
            "provider" | "providers" => match parts.as_slice() {
                [p, n, ..] => format!("provider.{p}.{n}"),
                _ => return,
            },
            _ => match declaration(&parts) {
                Some(key) => key,
                None => return,
            },
        };
        self.target(from, key);
    }

    /// A `uses` edge to the declared Struct `key`, else an unresolved `unknown` target.
    fn target(&mut self, from: usize, key: String) {
        match self.declared.get(&key) {
            Some(&to) => self.fact(Fact::Uses { from, to }),
            None => self.fact(Fact::Unresolved {
                from,
                name: key,
                category: "unknown",
            }),
        }
    }

    fn module_source(&mut self, from: usize, a: &Attribute) {
        match a.literal.as_deref() {
            Some(src) if src.starts_with("./") || src.starts_with("../") => {
                let path = normalise(self.dir, src);
                let escapes = path.split('/').any(|s| s == "..");
                let files: Vec<String> = match self.modules.get(&path) {
                    Some(m) if !escapes => m.base.iter().chain(&m.overrides).cloned().collect(),
                    _ => Vec::new(),
                };
                if files.is_empty() {
                    self.fact(Fact::Unresolved {
                        from,
                        name: path,
                        category: "unknown",
                    });
                }
                for path in files {
                    self.fact(Fact::FileUses {
                        from,
                        path,
                        kind: "module-source",
                    });
                }
            }
            Some(src) => self.fact(Fact::Unresolved {
                from,
                name: src.to_string(),
                category: "external",
            }),
            None => {
                let Some(p) = self.parsed.iter().find(|p| p.path == self.file) else {
                    return;
                };
                let raw = p.text.raw_offset(a.value.start);
                self.fact(Fact::Unresolved {
                    from,
                    name: format!("<nameless@{}:{raw}>", self.file),
                    category: "unknown",
                });
            }
        }
    }
}

/// The name parts of a traversal: its root, then each leading attribute access.
fn parts(t: &Traversal) -> Option<Vec<&str>> {
    let Expression::Variable(root) = &t.expr else {
        return None;
    };
    let mut parts = vec![root.as_str()];
    for op in &t.operators {
        match op.value() {
            TraversalOperator::GetAttr(id) => parts.push(id.as_str()),
            _ => break,
        }
    }
    Some(parts)
}

/// The declaration a traversal names, as written: `var.x`, `local.x`, `module.x`,
/// `data.T.N` or `T.N`; `None` for an evaluation-context value or a bare root.
fn declaration(parts: &[&str]) -> Option<String> {
    match parts {
        [k @ ("var" | "local" | "module"), n, ..] => Some(format!("{k}.{n}")),
        ["data", t, n, ..] => Some(format!("data.{t}.{n}")),
        [c, ..] if CONTEXT.contains(c) => None,
        [t, n, ..] => Some(format!("{t}.{n}")),
        _ => None,
    }
}

/// `src` (a `./` or `../` literal) joined onto the calling module's `dir`, with `.`
/// dropped and each `..` dropped with the segment before it. A `..` left over (above
/// the root) stays in the result.
fn normalise(dir: &str, src: &str) -> String {
    let mut out: Vec<&str> = dir.split('/').filter(|s| !s.is_empty()).collect();
    for seg in src.split('/') {
        match seg {
            "" | "." => {}
            ".." if out.last().is_some_and(|l| *l != "..") => {
                out.pop();
            }
            s => out.push(s),
        }
    }
    out.join("/")
}
