//! Procedure-name resolution: which paragraph or section a PERFORM or GO TO names. [`scopes`]
//! indexes every PROGRAM-ID (nested programs included, in pre-order) with its procedure names
//! in source order; a name resolves only inside its own program, since COBOL procedure names
//! are local to a program.

use crate::ast::{
    CallTarget, CompilationUnit, DataDivision, DataEntry, FileName, Name, ProcedureDivision,
    Program, QualifiedName, Section,
};
use crate::diag::Diagnostic;
use crate::source_map::SourceMap;

/// Whether a procedure name is a section or a paragraph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcKind {
    Section,
    Paragraph,
}

/// A section or paragraph of one program. `section` is the index of the section that owns a
/// paragraph; `last` is the index of the last procedure in its block (a section's last
/// paragraph, or the procedure itself).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proc {
    pub kind: ProcKind,
    pub name: String,
    pub section: Option<usize>,
    pub last: usize,
}

/// One PROGRAM-ID unit. `parent` indexes the enclosing program in [`scopes`]; `procs` lists its
/// procedure names in source order; `records` holds its 01-level records (see [`Scope::data`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scope {
    pub name: String,
    pub parent: Option<usize>,
    pub procs: Vec<Proc>,
    pub records: Vec<DataEntry>,
}

/// What a procedure name resolves to. `One` and `Range` index the scope's `procs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    One(usize),
    /// `PERFORM p THRU q`: the paragraphs from `p` to `q` in source order.
    Range(Vec<usize>),
    Undefined,
    Ambiguous,
}

/// Every program of `unit`, nested ones included, in pre-order: a program precedes its nested ones.
pub fn scopes(unit: &CompilationUnit) -> Vec<Scope> {
    let mut out = Vec::new();
    for program in &unit.programs {
        add_scope(&mut out, program, None);
    }
    out
}

fn add_scope(out: &mut Vec<Scope>, program: &Program, parent: Option<usize>) {
    let at = out.len();
    let mut procs = Vec::new();
    if let Some(div) = &program.procedure {
        procedures(&mut procs, div);
    }
    out.push(Scope {
        name: program.name.text.clone(),
        parent,
        procs,
        records: records(program.data.as_ref()),
    });
    for nested in &program.nested {
        add_scope(out, nested, Some(at));
    }
}

/// The non-data names one program declares, parallel to [`scopes`]: ENVIRONMENT names, FD/SD
/// files and the index names of its entries. Only [`declares`] reads it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Declared {
    pub names: Vec<Name>,
    pub files: Vec<FileName>,
    pub indexes: Vec<Name>,
}

/// One [`Declared`] per program of `unit`, in the same pre-order as [`scopes`].
pub fn declared(unit: &CompilationUnit) -> Vec<Declared> {
    let mut out = Vec::new();
    for program in &unit.programs {
        add_declared(&mut out, program);
    }
    out
}

fn add_declared(out: &mut Vec<Declared>, program: &Program) {
    let mut d = Declared {
        names: program.declared.clone(),
        ..Declared::default()
    };
    if let Some(data) = &program.data {
        d.files = data.files.clone();
        for section in &data.sections {
            index_names(&mut d.indexes, &section.entries);
        }
    }
    out.push(d);
    for nested in &program.nested {
        add_declared(out, nested);
    }
}

fn index_names(out: &mut Vec<Name>, entries: &[DataEntry]) {
    for entry in entries {
        out.extend(entry.indexes.iter().cloned());
        index_names(out, &entry.children);
    }
}

/// The named level-01 entries of every DATA DIVISION section, in source order; a repeated name
/// keeps its first declaration. Levels 66, 77 and 88 and subordinate items are not records.
fn records(data: Option<&DataDivision>) -> Vec<DataEntry> {
    let text = |e: &DataEntry| e.name.as_ref().map(|n| n.text.to_ascii_uppercase());
    let mut out: Vec<DataEntry> = Vec::new();
    let entries = data
        .into_iter()
        .flat_map(|d| &d.sections)
        .flat_map(|s| &s.entries);
    for entry in entries {
        if entry.level == 1 && text(entry).is_some() && !out.iter().any(|r| text(r) == text(entry))
        {
            out.push(entry.clone());
        }
    }
    out
}

/// Source order: declaratives, then unsectioned paragraphs, then sections.
fn procedures(out: &mut Vec<Proc>, div: &ProcedureDivision) {
    for section in &div.declaratives {
        add_section(out, section);
    }
    for paragraph in &div.paragraphs {
        push(out, ProcKind::Paragraph, &paragraph.name.text, None);
    }
    for section in &div.sections {
        add_section(out, section);
    }
}

fn add_section(out: &mut Vec<Proc>, section: &Section) {
    let at = push(out, ProcKind::Section, &section.name.text, None);
    for paragraph in &section.paragraphs {
        push(out, ProcKind::Paragraph, &paragraph.name.text, Some(at));
    }
    out[at].last = out.len() - 1;
}

fn push(out: &mut Vec<Proc>, kind: ProcKind, name: &str, section: Option<usize>) -> usize {
    let at = out.len();
    out.push(Proc {
        kind,
        name: name.to_string(),
        section,
        last: at,
    });
    at
}

impl Scope {
    /// Resolves a `PERFORM` or `GO TO` name: one procedure, `Undefined`, or `Ambiguous`.
    pub fn resolve(&self, name: &QualifiedName) -> Resolution {
        let hits: Vec<usize> = (0..self.procs.len())
            .filter(|&i| self.matches_name(i, name))
            .collect();
        match hits.as_slice() {
            [] => Resolution::Undefined,
            [one] => Resolution::One(*one),
            _ => Resolution::Ambiguous,
        }
    }

    /// Resolves `PERFORM from THRU thru`: the paragraphs from `from` to `thru` in source order.
    /// A section endpoint covers all of its paragraphs; an end before the start is empty.
    pub fn range(&self, from: &QualifiedName, thru: &QualifiedName) -> Resolution {
        let first = match self.resolve(from) {
            Resolution::One(i) => i,
            other => return other,
        };
        let last = match self.resolve(thru) {
            Resolution::One(j) => self.procs[j].last,
            other => return other,
        };
        Resolution::Range(
            (first..=last)
                .filter(|&k| self.procs[k].kind == ProcKind::Paragraph)
                .collect(),
        )
    }

    /// Resolves a data name to the index in `records` of the record holding it: a record's own
    /// name, or an item beneath it whose qualifiers name enclosing entries. Condition names (88)
    /// are not storage and never resolve. Zero or several records give `None`, never a guess.
    pub fn data(&self, name: &QualifiedName) -> Option<usize> {
        let hits: Vec<usize> = (0..self.records.len())
            .filter(|&i| holds(&self.records[i], name, &[], false, false))
            .collect();
        match hits.as_slice() {
            [one] => Some(*one),
            _ => None,
        }
    }

    /// The name matches, and each qualifier (innermost first) names an enclosing section or this program.
    fn matches_name(&self, i: usize, name: &QualifiedName) -> bool {
        let proc = &self.procs[i];
        if !proc.name.eq_ignore_ascii_case(&name.name.text) {
            return false;
        }
        let mut ancestors = proc
            .section
            .map(|s| self.procs[s].name.as_str())
            .into_iter()
            .chain(std::iter::once(self.name.as_str()));
        name.qualifiers
            .iter()
            .all(|q| ancestors.any(|a| a.eq_ignore_ascii_case(&q.text)))
    }
}

/// What a `CALL` target names. `Program` indexes [`scopes`]; `Ambiguous` is several programs
/// sharing the name; `External` is a literal naming no program of the unit; `Dynamic` is a data
/// name, which is never resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallResolution {
    Program(usize),
    Ambiguous,
    External,
    Dynamic,
}

/// Resolves a `CALL` target against the programs of one unit. A literal matches a PROGRAM-ID
/// exactly; failing that, case-insensitively, which draws a warning at the literal.
pub fn resolve_call(
    scopes: &[Scope],
    map: &SourceMap,
    target: &CallTarget,
) -> (CallResolution, Option<Diagnostic>) {
    let name = match target {
        CallTarget::Literal(name) => name,
        CallTarget::Identifier(_) => return (CallResolution::Dynamic, None),
    };
    let named = |exact: bool| -> Vec<usize> {
        scopes
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                if exact {
                    s.name == name.text
                } else {
                    s.name.eq_ignore_ascii_case(&name.text)
                }
            })
            .map(|(i, _)| i)
            .collect()
    };
    let exact = named(true);
    if !exact.is_empty() {
        return (call_hit(&exact), None);
    }
    let loose = named(false);
    let warning = (!loose.is_empty()).then(|| {
        map.diagnostic(
            name.span.start,
            &format!(
                "CALL '{}' matches a PROGRAM-ID only case-insensitively",
                name.text
            ),
        )
    });
    (call_hit(&loose), warning)
}

fn call_hit(hits: &[usize]) -> CallResolution {
    match hits {
        [] => CallResolution::External,
        [one] => CallResolution::Program(*one),
        _ => CallResolution::Ambiguous,
    }
}

/// Resolves `name` as seen from scope `idx` when its own records do not hold it: the GLOBAL
/// record or GLOBAL item of the nearest containing program that holds it, as `(scope, record)`.
/// A name the scope declares itself (see [`declares`]) shadows the inherited one, and so does a
/// local record holding it; an ambiguous match gives `None`, never a guess.
pub fn global_data(
    scopes: &[Scope],
    decls: &[Declared],
    idx: usize,
    name: &QualifiedName,
) -> Option<(usize, usize)> {
    let own = &scopes[idx];
    if declares(scopes, decls, idx, name)
        || own
            .records
            .iter()
            .any(|r| holds(r, name, &[], false, false))
    {
        return None;
    }
    let mut at = own.parent;
    while let Some(a) = at {
        let hits: Vec<usize> = (0..scopes[a].records.len())
            .filter(|&i| holds(&scopes[a].records[i], name, &[], true, false))
            .collect();
        match hits.as_slice() {
            [] => at = scopes[a].parent,
            [one] => return Some((a, *one)),
            _ => return None,
        }
    }
    None
}

/// Whether scope `idx` declares `name` as a non-data name: its own ENVIRONMENT name, file or
/// index, or a GLOBAL file of a containing program. A qualified reference is never one.
pub fn declares(scopes: &[Scope], decls: &[Declared], idx: usize, name: &QualifiedName) -> bool {
    let text = &name.name.text;
    let named = |n: &Name| n.text.eq_ignore_ascii_case(text);
    if !name.qualifiers.is_empty() {
        return false;
    }
    let own = &decls[idx];
    if own.names.iter().any(named)
        || own.indexes.iter().any(named)
        || own.files.iter().any(|f| named(&f.name))
    {
        return true;
    }
    let mut at = scopes[idx].parent;
    while let Some(a) = at {
        if decls[a].files.iter().any(|f| f.global && named(&f.name)) {
            return true;
        }
        at = scopes[a].parent;
    }
    false
}

/// Whether `entry` or an item beneath it is named `name`, with every qualifier among the names
/// `enclosing` (the entries above it, `entry` itself excluded). With `global_only`, an entry
/// counts only when it is GLOBAL or lies beneath a GLOBAL entry (`under_global`).
fn holds(
    entry: &DataEntry,
    name: &QualifiedName,
    enclosing: &[&str],
    global_only: bool,
    under_global: bool,
) -> bool {
    let own = entry
        .name
        .as_ref()
        .filter(|_| entry.level != 88)
        .map(|n| n.text.as_str());
    let visible = !global_only || under_global || entry.global;
    if visible
        && own.is_some_and(|n| n.eq_ignore_ascii_case(&name.name.text))
        && name
            .qualifiers
            .iter()
            .all(|q| enclosing.iter().any(|e| e.eq_ignore_ascii_case(&q.text)))
    {
        return true;
    }
    let below: Vec<&str> = enclosing.iter().copied().chain(own).collect();
    let inside = under_global || entry.global;
    entry
        .children
        .iter()
        .any(|c| holds(c, name, &below, global_only, inside))
}
