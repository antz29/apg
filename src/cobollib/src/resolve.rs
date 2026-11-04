//! Procedure-name resolution: which paragraph or section a PERFORM or GO TO names. [`scopes`]
//! indexes every PROGRAM-ID (nested programs included, in pre-order) with its procedure names
//! in source order; a name resolves only inside its own program, since COBOL procedure names
//! are local to a program.

use crate::ast::{
    CallTarget, Class, CompilationUnit, DataDivision, DataEntry, FileName, Interface, Method,
    MethodPrototype, Name, ProcedureDivision, Program, QualifiedName, Receiver, RepositoryEntry,
    Section,
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

/// A CLASS-ID or INTERFACE-ID of the scan, by reference. scan.rs matches it to its Struct by
/// address (`std::ptr::eq`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Type<'a> {
    Class(&'a Class),
    Interface(&'a Interface),
}

/// What a class or interface name resolves to. `External` is a name outside the scan (its
/// target is an unresolved use of category external); `Ambiguous` is several scanned types
/// sharing the name, which is never guessed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeRes<'a> {
    Scanned(Type<'a>),
    External,
    Ambiguous,
}

/// What an `INVOKE` resolves to: a METHOD-ID (`Method`), an interface prototype, a class outside
/// the scan (`External`), or nothing the scan can name (`Unknown`, so no target).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MethodRes<'a> {
    Method(&'a Method),
    Prototype(&'a MethodPrototype),
    External,
    Unknown,
}

/// Where an `INVOKE` is written: its enclosing class, whether the enclosing method sits in the
/// FACTORY section (`factory`), the data divisions searched for a receiver item in order
/// (innermost first), and the REPOSITORY entries in scope.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Site<'a> {
    pub class: Option<&'a Class>,
    pub factory: bool,
    pub data: Vec<&'a DataDivision>,
    pub repository: Vec<&'a RepositoryEntry>,
}

/// Every CLASS-ID and INTERFACE-ID of the scan, nested programs included, by COBOL name.
#[derive(Debug)]
pub struct ClassIndex<'a> {
    types: Vec<(&'a str, Type<'a>)>,
}

impl<'a> ClassIndex<'a> {
    /// Indexes the classes and interfaces of `units`.
    pub fn new(units: impl IntoIterator<Item = &'a CompilationUnit>) -> Self {
        let mut types = Vec::new();
        for unit in units {
            for class in &unit.classes {
                types.push((class.name.text.as_str(), Type::Class(class)));
            }
            for interface in &unit.interfaces {
                types.push((interface.name.text.as_str(), Type::Interface(interface)));
            }
            for program in &unit.programs {
                add_program(&mut types, program);
            }
        }
        Self { types }
    }

    /// Resolves a class or interface name, case-insensitively as COBOL words are.
    pub fn resolve_type(&self, name: &Name) -> TypeRes<'a> {
        let hits: Vec<Type<'a>> = self
            .types
            .iter()
            .filter(|(n, _)| n.eq_ignore_ascii_case(&name.text))
            .map(|(_, t)| *t)
            .collect();
        match hits.as_slice() {
            [] => TypeRes::External,
            [one] => TypeRes::Scanned(*one),
            _ => TypeRes::Ambiguous,
        }
    }

    /// Resolves `INVOKE receiver method`. `SELF` is the enclosing class (instance or factory by
    /// `site.factory`); `SUPER` its first parent; an object data item its `OBJECT REFERENCE`
    /// class (instance methods); a class name its factory methods. Parents are walked through
    /// `INHERITS`. Method names match case-insensitively; a dynamic method name is `Unknown`.
    pub fn resolve_invoke(
        &self,
        site: &Site<'a>,
        receiver: &Receiver,
        method: &CallTarget,
    ) -> MethodRes<'a> {
        let CallTarget::Literal(name) = method else {
            return MethodRes::Unknown;
        };
        let budget = self.types.len() + 1;
        match receiver {
            Receiver::Self_ => site.class.map_or(MethodRes::Unknown, |c| {
                self.find(Type::Class(c), &name.text, site.factory, budget)
            }),
            Receiver::Super => site
                .class
                .and_then(|c| c.inherits.first())
                .map_or(MethodRes::Unknown, |p| {
                    self.via(self.resolve_type(p), &name.text, site.factory, budget)
                }),
            Receiver::Null => MethodRes::Unknown,
            Receiver::ClassName(class) => self.class_receiver(site, class, &name.text, budget),
            Receiver::Name(qn) => match item_class(&site.data, qn) {
                Some(Some(class)) => self.via(self.resolve_type(class), &name.text, false, budget),
                Some(None) => MethodRes::Unknown,
                None if qn.qualifiers.is_empty() => {
                    self.class_receiver(site, &qn.name, &name.text, budget)
                }
                None => MethodRes::Unknown,
            },
        }
    }

    /// `INVOKE class "m"`: a scanned class's factory method; a REPOSITORY class outside the scan
    /// is `External`; any other name is `Unknown`.
    fn class_receiver(
        &self,
        site: &Site<'a>,
        class: &Name,
        method: &str,
        budget: usize,
    ) -> MethodRes<'a> {
        match self.resolve_type(class) {
            TypeRes::Scanned(Type::Class(c)) => self.find(Type::Class(c), method, true, budget),
            TypeRes::Scanned(Type::Interface(_)) | TypeRes::Ambiguous => MethodRes::Unknown,
            TypeRes::External
                if site
                    .repository
                    .iter()
                    .any(|r| r.name.text.eq_ignore_ascii_case(&class.text)) =>
            {
                MethodRes::External
            }
            TypeRes::External => MethodRes::Unknown,
        }
    }

    fn via(&self, res: TypeRes<'a>, method: &str, factory: bool, budget: usize) -> MethodRes<'a> {
        match res {
            TypeRes::Scanned(t) => self.find(t, method, factory, budget),
            TypeRes::External => MethodRes::External,
            TypeRes::Ambiguous => MethodRes::Unknown,
        }
    }

    /// Finds `method` in `ty` or, failing that, in its parents; `budget` bounds an INHERITS cycle.
    fn find(&self, ty: Type<'a>, method: &str, factory: bool, budget: usize) -> MethodRes<'a> {
        if budget == 0 {
            return MethodRes::Unknown;
        }
        let budget = budget - 1;
        let (own, parents) = match ty {
            Type::Class(c) => {
                let own = c
                    .factory
                    .iter()
                    .chain(c.object.iter())
                    .flat_map(|s| &s.methods)
                    .find(|m| m.factory == factory && m.name.text.eq_ignore_ascii_case(method))
                    .map(MethodRes::Method);
                (own, &c.inherits)
            }
            Type::Interface(i) => {
                let own = i
                    .prototypes
                    .iter()
                    .find(|p| p.name.text.eq_ignore_ascii_case(method))
                    .map(MethodRes::Prototype);
                (own, &i.inherits)
            }
        };
        own.or_else(|| {
            parents
                .iter()
                .map(|p| self.via(self.resolve_type(p), method, factory, budget))
                .find(|r| *r != MethodRes::Unknown)
        })
        .unwrap_or(MethodRes::Unknown)
    }
}

fn add_program<'a>(types: &mut Vec<(&'a str, Type<'a>)>, program: &'a Program) {
    for class in &program.classes {
        types.push((class.name.text.as_str(), Type::Class(class)));
    }
    for nested in &program.nested {
        add_program(types, nested);
    }
}

/// The class an object data item names, for a receiver: `None` when no single item holds the
/// name, `Some(None)` for an untyped `OBJECT REFERENCE`, `Some(Some(class))` for a typed one.
fn item_class<'d>(data: &[&'d DataDivision], name: &QualifiedName) -> Option<Option<&'d Name>> {
    let mut hits: Vec<&'d DataEntry> = Vec::new();
    for &div in data {
        for section in &div.sections {
            items(&section.entries, name, &[], &mut hits);
        }
    }
    match hits.as_slice() {
        [one] => {
            let entry: &'d DataEntry = one;
            Some(
                entry
                    .object_reference
                    .as_ref()
                    .and_then(|o| o.class.as_ref()),
            )
        }
        _ => None,
    }
}

/// Collects the entries named `name` (qualifiers among `enclosing`), recursing into children.
fn items<'d>(
    entries: &'d [DataEntry],
    name: &QualifiedName,
    enclosing: &[&str],
    out: &mut Vec<&'d DataEntry>,
) {
    for entry in entries {
        let own = entry
            .name
            .as_ref()
            .filter(|_| entry.level != 88)
            .map(|n| n.text.as_str());
        if own.is_some_and(|n| n.eq_ignore_ascii_case(&name.name.text))
            && name
                .qualifiers
                .iter()
                .all(|q| enclosing.iter().any(|e| e.eq_ignore_ascii_case(&q.text)))
        {
            out.push(entry);
        }
        let below: Vec<&str> = enclosing.iter().copied().chain(own).collect();
        items(&entry.children, name, &below, out);
    }
}
