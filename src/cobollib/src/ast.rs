//! Syntax tree for COBOL-85 source: the types the parser submodules build and the fact
//! mapping reads. Every node is located through `Origin`, so a node declared in a copybook
//! keeps the copybook's file. Names are kept as written; COBOL matching is case-insensitive.

use crate::source_map::Origin;

/// A range in the original text: the origin of its first and last token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: Origin,
    pub end: Origin,
}

/// A word as written, with where it sits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Name {
    pub text: String,
    pub span: Span,
}

/// A data or procedure name with its `OF`/`IN` qualifiers, innermost first (`p OF s OF t`
/// is `p` with qualifiers `[s, t]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QualifiedName {
    pub name: Name,
    pub qualifiers: Vec<Name>,
}

/// One parsed file: the top-level programs it contains.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompilationUnit {
    pub programs: Vec<Program>,
    /// Top-level `CLASS-ID` units (COBOL 2002 OO).
    pub classes: Vec<Class>,
    /// Top-level `INTERFACE-ID` units (COBOL 2002 OO).
    pub interfaces: Vec<Interface>,
}

/// A `PROGRAM-ID` unit with its divisions and nested programs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Program {
    pub name: Name,
    pub span: Span,
    pub identification: Span,
    pub environment: Option<Span>,
    pub data: Option<DataDivision>,
    pub procedure: Option<ProcedureDivision>,
    pub nested: Vec<Program>,
    /// Non-data names the program declares in its ENVIRONMENT DIVISION: SELECT file names,
    /// SPECIAL-NAMES mnemonic names, ALPHABET, CLASS and ON/OFF STATUS names.
    pub declared: Vec<Name>,
    /// Entries of the CONFIGURATION SECTION's REPOSITORY paragraph (COBOL 2002 OO).
    pub repository: Vec<RepositoryEntry>,
    /// `CLASS-ID` units nested in this program (COBOL 2002 OO).
    pub classes: Vec<Class>,
}

/// The DATA DIVISION and its sections.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataDivision {
    pub span: Span,
    pub sections: Vec<DataSection>,
    /// FD and SD file names, each with its `GLOBAL` flag.
    pub files: Vec<FileName>,
}

/// An FD or SD file name and whether its header carries `GLOBAL`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileName {
    pub name: Name,
    pub global: bool,
}

/// Which DATA DIVISION section holds its entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataSectionKind {
    File,
    WorkingStorage,
    LocalStorage,
    Linkage,
}

/// A DATA DIVISION section and its top-level entries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataSection {
    pub kind: DataSectionKind,
    pub span: Span,
    pub entries: Vec<DataEntry>,
}

/// A data description entry (level 01-49, 66, 77 or 88). Entries nest by level: `children`
/// holds the entries subordinate to this one, including level-88 condition names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataEntry {
    pub level: u8,
    /// `None` for `FILLER`.
    pub name: Option<Name>,
    pub span: Span,
    pub redefines: bool,
    pub renames: bool,
    pub occurs: bool,
    /// The entry carries the `GLOBAL` clause.
    pub global: bool,
    /// Index names from `INDEXED [BY]`.
    pub indexes: Vec<Name>,
    pub pic: Option<Pic>,
    /// The `OBJECT REFERENCE` clause, when the entry names a class or interface.
    pub object_reference: Option<ObjectReference>,
    /// The Micro Focus `PROPERTY` clause, when the entry is a property.
    pub property: Option<Property>,
    pub children: Vec<DataEntry>,
}

/// A `PIC` clause as written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pic {
    pub text: String,
    pub span: Span,
}

/// The PROCEDURE DIVISION: declaratives, unsectioned paragraphs, then sections.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcedureDivision {
    pub span: Span,
    pub declaratives: Vec<Section>,
    pub paragraphs: Vec<Paragraph>,
    pub sections: Vec<Section>,
}

/// A procedure section (declarative sections included) and its paragraphs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub name: Name,
    pub span: Span,
    pub paragraphs: Vec<Paragraph>,
}

/// A paragraph and its sentences.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paragraph {
    pub name: Name,
    pub span: Span,
    pub sentences: Vec<Sentence>,
}

/// A sentence: its statements and the period that ends it, if written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sentence {
    pub span: Span,
    pub statements: Vec<Statement>,
    pub period: Option<Origin>,
}

/// A procedure statement. `children` holds statements nested in its branches (IF, ELSE,
/// EVALUATE WHEN, ON SIZE ERROR, ...), in source order; the branch boundaries are not kept.
/// `scope_end` is the explicit scope terminator (`END-IF`, `END-PERFORM`, ...) when written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Statement {
    pub span: Span,
    pub scope_end: Option<Span>,
    pub transfer: Option<Transfer>,
    /// Data-name references in the statement's operands (`MOVE`, `READ ... INTO`, ...).
    pub refs: Vec<QualifiedName>,
    pub children: Vec<Statement>,
}

/// Control transfer carried by a statement's operands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transfer {
    /// `PERFORM` target; `None` for an inline perform.
    Perform(Option<ProcRange>),
    GoTo(Vec<QualifiedName>),
    /// `RESUME AT procedure-name`.
    Resume(QualifiedName),
    Call(CallTarget),
    /// `EXEC CICS INVOKE SERVICE` / `WEBSERVICE`: the target lies outside the scan.
    ExecService(CallTarget),
    /// `SORT` or `MERGE` with their `INPUT`/`OUTPUT PROCEDURE` ranges.
    SortMerge {
        input: Option<ProcRange>,
        output: Option<ProcRange>,
    },
    /// `INVOKE receiver method [USING ...] [RETURNING ...]` (OO COBOL).
    Invoke {
        receiver: Receiver,
        method: CallTarget,
        using: Vec<QualifiedName>,
        returning: Option<QualifiedName>,
    },
    /// `RAISE` of a condition name or of an object (OO COBOL and Micro Focus).
    Raise(RaiseTarget),
}

/// The receiver of an `INVOKE`: a class name (factory), `SELF`, `SUPER`, `NULL`, or an
/// object reference data item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Receiver {
    Self_,
    Super,
    Null,
    Name(QualifiedName),
    ClassName(Name),
}

/// What a `RAISE` names: a standard condition name, or an object reference data item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RaiseTarget {
    ExceptionName(Name),
    Object(QualifiedName),
}

/// A `CLASS-ID` unit with its inheritance, repository entries and FACTORY/OBJECT sections.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Class {
    pub name: Name,
    pub span: Span,
    pub identification: Span,
    /// The `AS "..."` literal naming the class outside COBOL.
    pub external: Option<Name>,
    pub inherits: Vec<Name>,
    pub implements: Vec<Name>,
    pub repository: Vec<RepositoryEntry>,
    pub factory: Option<ObjectSection>,
    pub object: Option<ObjectSection>,
}

/// The FACTORY or OBJECT section of a class: its data and its methods.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectSection {
    pub span: Span,
    pub data: Option<DataDivision>,
    pub methods: Vec<Method>,
}

/// An `INTERFACE-ID` unit with the interfaces it `INHERITS FROM` and its method prototypes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Interface {
    pub name: Name,
    pub span: Span,
    pub identification: Span,
    pub inherits: Vec<Name>,
    /// Parameter class and interface names of `INTERFACE-ID … USING Param`.
    pub using: Vec<Name>,
    pub prototypes: Vec<MethodPrototype>,
}

/// A `METHOD-ID` unit. `factory` is set from the FACTORY section it sits in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Method {
    pub name: Name,
    pub span: Span,
    pub factory: bool,
    pub data: Option<DataDivision>,
    pub procedure: Option<ProcedureDivision>,
}

/// A method prototype in an interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MethodPrototype {
    pub name: Name,
    pub span: Span,
}

/// A `REPOSITORY` entry: `CLASS name [AS "external"]` or `INTERFACE name`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryEntry {
    pub name: Name,
    pub external: Option<Name>,
}

/// The `OBJECT REFERENCE [class]` clause of a data item; `class` is `None` when untyped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectReference {
    pub class: Option<Name>,
}

/// The Micro Focus `PROPERTY [AS "name"] [WITH NO SET]` clause of a data item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Property {
    pub as_name: Option<Name>,
    pub no_set: bool,
}

/// A procedure name, or `from THRU thru`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcRange {
    pub from: QualifiedName,
    pub thru: Option<QualifiedName>,
}

/// The target of `CALL`: a literal program name or a data item (dynamic).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallTarget {
    Literal(Name),
    Identifier(QualifiedName),
}
