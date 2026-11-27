//! Declaration scanner: turns one parsed Bicep file into the Struct and Function
//! units the emitter streams. Pure over parsed input: the caller parses the file
//! and writes the duplicate diagnostics. Example: `resource stg '…' = {` is the
//! Struct `resource.stg`; `func f() string` is the Function `func.f`.

use std::collections::HashSet;

use crate::parse::{Decl, File, ImportSym, Kind};
use crate::source::Span;

/// One Struct or Function the file declares. `parent` indexes `Units::units`.
pub struct Unit<'a> {
    /// `<keyword>.<name>` (`resource.stg`, `func.f`, `import.x`), `using` or
    /// `extends` for those statements, or a child resource's bare symbolic name.
    pub name: String,
    /// The declaration statement's span (an import symbol shares its statement's span).
    pub span: Span,
    pub parent: Option<usize>,
    pub function: bool,
    pub decl: &'a Decl,
    /// The symbol an `import` binds; `None` for every other unit.
    pub import: Option<&'a ImportSym>,
}

/// A repeated name in one scope: the earlier declaration is kept, this one dropped.
pub struct Duplicate {
    pub line: usize,
    pub name: String,
}

#[derive(Default)]
pub struct Units<'a> {
    pub units: Vec<Unit<'a>>,
    pub duplicates: Vec<Duplicate>,
}

/// Scans one parsed file's declarations. A duplicate's child resources are
/// dropped with it, since they have no parent unit to hang under.
pub fn file_units(file: &File) -> Units<'_> {
    let mut out = Units::default();
    let mut seen = HashSet::new();
    for decl in &file.decls {
        scan(decl, None, &mut seen, &mut out);
    }
    out
}

fn scan<'a>(
    decl: &'a Decl,
    parent: Option<usize>,
    seen: &mut HashSet<(Option<usize>, String)>,
    out: &mut Units<'a>,
) {
    for (name, import) in names_of(decl, parent.is_some()) {
        if !seen.insert((parent, name.clone())) {
            out.duplicates.push(Duplicate {
                line: decl.span.start_line,
                name,
            });
            continue;
        }
        let index = out.units.len();
        out.units.push(Unit {
            name,
            span: decl.span,
            parent,
            function: decl.kind == Kind::Func,
            decl,
            import,
        });
        for child in &decl.children {
            scan(child, Some(index), seen, out);
        }
    }
}

/// The unit names one declaration yields: one per import symbol, else one.
fn names_of(decl: &Decl, child: bool) -> Vec<(String, Option<&ImportSym>)> {
    if decl.kind == Kind::Import {
        return decl
            .imports
            .iter()
            .map(|s| (format!("import.{}", s.local.text), Some(s)))
            .collect();
    }
    let kw = keyword(decl.kind);
    match &decl.name {
        None => vec![(kw.to_string(), None)],
        Some(n) if child => vec![(n.text.clone(), None)],
        Some(n) => vec![(format!("{kw}.{}", n.text), None)],
    }
}

fn keyword(kind: Kind) -> &'static str {
    match kind {
        Kind::Param => "param",
        Kind::Var => "var",
        Kind::Resource => "resource",
        Kind::Module => "module",
        Kind::Output => "output",
        Kind::Type => "type",
        Kind::Func => "func",
        Kind::Import => "import",
        Kind::Using => "using",
        Kind::Extends => "extends",
    }
}
