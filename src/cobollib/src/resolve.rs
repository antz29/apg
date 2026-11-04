//! Procedure-name resolution: which paragraph or section a PERFORM or GO TO names. [`scopes`]
//! indexes every PROGRAM-ID (nested programs included, in pre-order) with its procedure names
//! in source order; a name resolves only inside its own program, since COBOL procedure names
//! are local to a program.

use crate::ast::{CompilationUnit, ProcedureDivision, Program, QualifiedName, Section};

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
/// procedure names in source order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scope {
    pub name: String,
    pub parent: Option<usize>,
    pub procs: Vec<Proc>,
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
    });
    for nested in &program.nested {
        add_scope(out, nested, Some(at));
    }
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
