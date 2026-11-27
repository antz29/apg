//! Reference resolver: turns the declaration units of every scanned Bicep file
//! into the uses, calls, file-dependency and unresolved facts the scan emits.
//! Pure over parsed input: the caller passes every scanned file (an import or a
//! module path reaches across files) and writes the records. Example: in
//! `resource app '…' = { parent: plan }` the `resource.app` Struct gets one
//! `Uses` edge to `resource.plan`; `module m './net.bicep'` is one `FileUses`
//! of kind `module` to `net.bicep`.

use std::collections::{HashMap, HashSet};

use crate::parse::{Kind, Name, Ref};
use crate::units::{Unit, Units};

/// One scanned file: its repo-relative `/`-separated path and its declaration units.
/// Build it from a parsed file with `units::file_units`.
pub struct Source<'a> {
    pub path: &'a str,
    pub units: Units<'a>,
}

/// One resolved fact. `from` indexes the units of the file the facts are returned for;
/// `file` and the `to` of a `Uses` index `resolve`'s `sources` slice and that file's units.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Fact {
    /// A `uses` edge from a Struct or Function to the Struct `to` of file `file`.
    Uses { from: usize, file: usize, to: usize },
    /// A `calls` edge from a Function to a Function of the same file.
    Calls { from: usize, to: usize },
    /// A Struct-to-File `uses` edge of kind `module`, `using`, `extends` or `import`; `path` is repo-relative.
    FileUses {
        from: usize,
        path: String,
        kind: &'static str,
    },
    /// A target the scan cannot reach: category `unknown` (a missing or escaping
    /// local path, a computed path) or `external` (a registry reference, named as written).
    Unresolved {
        from: usize,
        name: String,
        category: &'static str,
    },
}

/// Resolves every file's facts. The result is aligned with `sources`: entry `i`
/// holds the facts whose `from` indexes `sources[i].units.units`.
pub fn resolve(sources: &[Source<'_>]) -> Vec<Vec<Fact>> {
    let index: Vec<Index> = sources.iter().map(index_of).collect();
    (0..sources.len())
        .map(|file| {
            let mut r = Resolver {
                sources,
                index: &index,
                file,
                facts: Vec::new(),
                seen: HashSet::new(),
            };
            for (i, u) in sources[file].units.units.iter().enumerate() {
                r.unit(i, u);
            }
            r.facts
        })
        .collect()
}

/// A file's top-level names, each mapped to its unit index (first declaration wins).
#[derive(Default)]
struct Index {
    /// Symbols a reference may name: declarations and import-bound locals.
    symbols: HashMap<String, usize>,
    functions: HashMap<String, usize>,
    /// Type and var declarations an `import` of the name reaches.
    exports: HashMap<String, usize>,
}

fn index_of(src: &Source<'_>) -> Index {
    let mut ix = Index::default();
    for (i, u) in src.units.units.iter().enumerate() {
        if u.parent.is_some() {
            continue;
        }
        let name = if u.decl.kind == Kind::Import {
            u.import.map(|s| s.local.text.clone())
        } else {
            u.decl.name.as_ref().map(|n| n.text.clone())
        };
        let Some(name) = name else { continue };
        if u.function {
            ix.functions.entry(name).or_insert(i);
            continue;
        }
        if matches!(u.decl.kind, Kind::Type | Kind::Var) {
            ix.exports.entry(name.clone()).or_insert(i);
        }
        ix.symbols.entry(name).or_insert(i);
    }
    ix
}

struct Resolver<'s, 'a> {
    sources: &'s [Source<'a>],
    index: &'s [Index],
    file: usize,
    facts: Vec<Fact>,
    seen: HashSet<Fact>,
}

impl Resolver<'_, '_> {
    fn fact(&mut self, f: Fact) {
        if self.seen.insert(f.clone()) {
            self.facts.push(f);
        }
    }

    fn path(&self) -> &str {
        self.sources[self.file].path
    }

    fn unit(&mut self, from: usize, u: &Unit<'_>) {
        self.path_use(from, u);
        for r in &u.decl.refs {
            self.reference(from, u.function, r);
        }
    }

    /// The file edge or unresolved target of a module, using, extends or import path.
    fn path_use(&mut self, from: usize, u: &Unit<'_>) {
        let kind = match u.decl.kind {
            Kind::Module => "module",
            Kind::Using => "using",
            Kind::Extends => "extends",
            Kind::Import => "import",
            _ => return,
        };
        // `using none` carries no literal and draws no edge.
        let Some(lit) = &u.decl.literal else { return };
        let Some(src) = &lit.value else {
            let name = format!("<nameless@{}:{}>", self.path(), lit.span.start);
            self.fact(Fact::Unresolved {
                from,
                name,
                category: "unknown",
            });
            return;
        };
        if registry(src) {
            self.fact(Fact::Unresolved {
                from,
                name: src.clone(),
                category: "external",
            });
            return;
        }
        self.local(from, kind, src, u);
    }

    /// A local path, normalised against the referencing file's directory, to the
    /// scanned file it names; anything else is an unresolved `unknown` target.
    fn local(&mut self, from: usize, kind: &'static str, src: &str, u: &Unit<'_>) {
        let path = if src.starts_with('/') {
            src.to_string()
        } else {
            normalise(directory(self.path()), src)
        };
        let escapes = src.starts_with('/') || path.split('/').any(|s| s == "..");
        let target = if escapes {
            None
        } else {
            self.sources.iter().position(|s| s.path == path)
        };
        let fits = match kind {
            "module" => has_suffix(&path, &[".bicep", ".json"]),
            "extends" => has_suffix(&path, &[".bicepparam"]),
            _ => true,
        };
        match target.filter(|_| fits) {
            Some(file) => {
                self.fact(Fact::FileUses { from, path, kind });
                if kind == "import" {
                    self.import_symbol(from, file, u);
                }
            }
            None => self.fact(Fact::Unresolved {
                from,
                name: path,
                category: "unknown",
            }),
        }
    }

    /// The uses edge from an import Struct to the exported type or var it names in `file`.
    fn import_symbol(&mut self, from: usize, file: usize, u: &Unit<'_>) {
        let Some(remote) = u.import.and_then(|s| s.remote.as_ref()) else {
            return;
        };
        let ix: &[Index] = self.index;
        if let Some(&to) = ix[file].exports.get(&remote.text) {
            self.fact(Fact::Uses { from, file, to });
        }
    }

    /// A reference: a call to a func of this file, or a symbol (with its `::` accessors).
    /// Local binders and names the file does not declare (built-ins, `az`, `sys`) draw nothing.
    fn reference(&mut self, from: usize, in_func: bool, r: &Ref) {
        if r.local {
            return;
        }
        let ix: &[Index] = self.index;
        let mine = &ix[self.file];
        if r.call {
            if in_func && r.accessors.is_empty() {
                if let Some(&to) = mine.functions.get(&r.name) {
                    self.fact(Fact::Calls { from, to });
                }
            }
            return;
        }
        let Some(&symbol) = mine.symbols.get(&r.name) else {
            return;
        };
        let to = self.descend(symbol, &r.accessors);
        self.fact(Fact::Uses {
            from,
            file: self.file,
            to,
        });
    }

    /// Follows `a::b::c` through child resources; stops at the deepest child found.
    fn descend(&self, mut cur: usize, accessors: &[Name]) -> usize {
        let units = &self.sources[self.file].units.units;
        for acc in accessors {
            match units
                .iter()
                .position(|c| c.parent == Some(cur) && c.name == acc.text)
            {
                Some(child) => cur = child,
                None => break,
            }
        }
        cur
    }
}

/// A registry reference: `br:`, `br/<alias>:`, `ts:` or `ts/<alias>:`.
pub(crate) fn registry(src: &str) -> bool {
    let Some((head, _)) = src.split_once(':') else {
        return false;
    };
    head == "br" || head == "ts" || head.starts_with("br/") || head.starts_with("ts/")
}

fn has_suffix(path: &str, suffixes: &[&str]) -> bool {
    let lower = path.to_ascii_lowercase();
    suffixes.iter().any(|s| lower.ends_with(s))
}

/// The directory part of a repo-relative file path (empty at the root).
pub fn directory(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

/// `src` joined onto `dir`, with `.` dropped and each `..` dropped with the segment
/// before it. A `..` left over (above the root) stays in the result.
pub fn normalise(dir: &str, src: &str) -> String {
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
