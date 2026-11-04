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
}

/// The DATA DIVISION and its sections.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataDivision {
    pub span: Span,
    pub sections: Vec<DataSection>,
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
    pub pic: Option<Pic>,
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
    /// `SORT` or `MERGE` with their `INPUT`/`OUTPUT PROCEDURE` ranges.
    SortMerge {
        input: Option<ProcRange>,
        output: Option<ProcRange>,
    },
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
