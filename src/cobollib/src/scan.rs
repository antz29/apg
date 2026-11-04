//! Structural facts for the unified JSONL stream: a module per COBOL directory, a File per
//! source file, programs (and nested programs), sections, paragraphs and level-01 records as
//! Structs and Functions, with `contains` edges by scanner-local opaque id. Byte offsets are
//! exact over the origin file's text, so a declaration in a copybook indexes the copybook.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write};
use std::path::Path;

use crate::ast::{Paragraph, Program, Section, Span};
use crate::diag::Diagnostic;
use crate::dialect::Dialect;
use crate::parser::parse_compilation_unit;
use crate::preprocess::{preprocess, Search};
use crate::source_map::{FileId, Origin, SourceMap};

/// Unified JSONL records (SPEC §2), the `cobol` stream's wire shapes.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Rec {
    Module {
        fqn: String,
    },
    File {
        path: String,
        parent: String,
        start_line: usize,
        end_line: usize,
    },
    Struct {
        id: String,
        parent: String,
        name: String,
        path: String,
        start: usize,
        end: usize,
        start_line: usize,
        end_line: usize,
    },
    Function {
        id: String,
        parent: String,
        name: String,
        file: String,
        path: String,
        start: usize,
        end: usize,
        start_line: usize,
        end_line: usize,
    },
    Contains {
        from: String,
        to: String,
    },
}

/// Accumulates one scan's records. Ids are assigned in emission order (`n1`, `n2`, ...).
#[derive(Debug, Default)]
pub struct Emitter {
    recs: Vec<Rec>,
    modules: BTreeSet<String>,
    next_id: usize,
}

/// A located span: the start origin's file name, byte offsets, and 1-based lines.
struct Loc {
    path: String,
    start: usize,
    end: usize,
    start_line: usize,
    end_line: usize,
}

/// One file's text with its line-start offsets, for `(line, col)` to byte conversion.
struct Text {
    src: String,
    starts: Vec<usize>,
}

impl Text {
    fn new(src: String) -> Self {
        let mut starts = vec![0];
        starts.extend(src.match_indices('\n').map(|(i, _)| i + 1));
        Text { src, starts }
    }

    /// Byte offset of 1-based `line` and `col`; `col` counts characters.
    fn offset(&self, line: usize, col: usize) -> usize {
        let start = self.starts[line - 1];
        start
            + self.src[start..]
                .chars()
                .take(col - 1)
                .map(char::len_utf8)
                .sum::<usize>()
    }

    /// Byte offset just past 1-based `line`, excluding its line break.
    fn line_end(&self, line: usize) -> usize {
        let start = self.starts[line - 1];
        let mut end = self
            .starts
            .get(line)
            .map_or(self.src.len(), |next| next - 1);
        if self.src[start..end].ends_with('\r') {
            end -= 1;
        }
        end
    }
}

/// Locates spans by their origin file's text, reading copybook text through `lookup`.
struct Locate<'a> {
    map: &'a SourceMap,
    main: &'a str,
    main_text: &'a str,
    lookup: &'a dyn Fn(&Path) -> Option<String>,
    texts: BTreeMap<String, Text>,
}

impl Locate<'_> {
    fn text(&mut self, file: FileId) -> &Text {
        let name = self.map.name(file).to_string();
        let (main, main_text, lookup) = (self.main, self.main_text, self.lookup);
        self.texts.entry(name.clone()).or_insert_with(|| {
            let src = if name == main {
                main_text.to_string()
            } else {
                lookup(Path::new(&name)).unwrap_or_default()
            };
            Text::new(src)
        })
    }

    fn span(&mut self, span: Span) -> Loc {
        let Origin {
            file,
            line: start_line,
            col,
        } = span.start;
        let path = self.map.name(file).to_string();
        let start = self.text(file).offset(start_line, col);
        let end_origin = span.end;
        let end_text = self.text(end_origin.file);
        let end = end_text.line_end(end_origin.line);
        Loc {
            path,
            start,
            end,
            start_line,
            end_line: end_origin.line,
        }
    }
}

/// The module identity of `path`: its repo-relative directory, `.` at the root.
fn module_of(path: &str) -> String {
    match path.rfind('/') {
        Some(i) => path[..i].to_string(),
        None => ".".to_string(),
    }
}

impl Emitter {
    fn fresh_id(&mut self) -> String {
        self.next_id += 1;
        format!("n{}", self.next_id)
    }

    /// Emits a Struct under `parent` (its scope), contained by `up` when given. Returns its
    /// id and the scope its own children are parented to.
    fn structure(
        &mut self,
        loc: &mut Locate,
        name: &str,
        span: Span,
        parent: &str,
        up: Option<&str>,
    ) -> (String, String) {
        let l = loc.span(span);
        let id = self.fresh_id();
        self.recs.push(Rec::Struct {
            id: id.clone(),
            parent: parent.to_string(),
            name: name.to_string(),
            path: l.path,
            start: l.start,
            end: l.end,
            start_line: l.start_line,
            end_line: l.end_line,
        });
        if let Some(up) = up {
            self.recs.push(Rec::Contains {
                from: up.to_string(),
                to: id.clone(),
            });
        }
        (id, format!("{parent}.{name}"))
    }

    fn program(&mut self, loc: &mut Locate, p: &Program, parent: &str, up: Option<&str>) {
        let (id, scope) = self.structure(loc, &p.name.text, p.span, parent, up);
        if let Some(data) = &p.data {
            for entry in data.sections.iter().flat_map(|s| &s.entries) {
                if let (1, Some(name)) = (entry.level, &entry.name) {
                    self.structure(loc, &name.text, entry.span, &scope, Some(&id));
                }
            }
        }
        if let Some(proc) = &p.procedure {
            for section in proc.declaratives.iter().chain(&proc.sections) {
                self.section(loc, section, &scope, &id);
            }
            for para in &proc.paragraphs {
                self.paragraph(loc, para, &scope, &id);
            }
        }
        for nested in &p.nested {
            self.program(loc, nested, &scope, Some(&id));
        }
    }

    fn section(&mut self, loc: &mut Locate, s: &Section, parent: &str, up: &str) {
        let (id, scope) = self.structure(loc, &s.name.text, s.span, parent, Some(up));
        for para in &s.paragraphs {
            self.paragraph(loc, para, &scope, &id);
        }
    }

    fn paragraph(&mut self, loc: &mut Locate, para: &Paragraph, parent: &str, up: &str) {
        let l = loc.span(para.span);
        let id = self.fresh_id();
        self.recs.push(Rec::Function {
            id: id.clone(),
            parent: parent.to_string(),
            name: para.name.text.clone(),
            file: l.path.clone(),
            path: l.path,
            start: l.start,
            end: l.end,
            start_line: l.start_line,
            end_line: l.end_line,
        });
        self.recs.push(Rec::Contains {
            from: up.to_string(),
            to: id,
        });
    }

    /// Scans one COBOL file: `path` is the identity its records carry and the name copybooks
    /// are searched from; `text` is its source. Returns the diagnostics of the expansion and
    /// parse, which are not emitted as records.
    pub fn scan_file(
        &mut self,
        path: &str,
        text: &str,
        dialect: Dialect,
        search: &Search,
        lookup: &dyn Fn(&Path) -> Option<String>,
    ) -> Vec<Diagnostic> {
        let module = module_of(path);
        if self.modules.insert(module.clone()) {
            self.recs.push(Rec::Module {
                fqn: module.clone(),
            });
        }
        self.recs.push(Rec::File {
            path: path.to_string(),
            parent: module.clone(),
            start_line: 1,
            end_line: text.lines().count().max(1),
        });
        let mut map = SourceMap::default();
        let (tokens, mut diags) = preprocess(&mut map, path, text, dialect, search, lookup);
        let (unit, more) = parse_compilation_unit(&map, dialect, &tokens);
        diags.extend(more);
        let mut loc = Locate {
            map: &map,
            main: path,
            main_text: text,
            lookup,
            texts: BTreeMap::new(),
        };
        for program in &unit.programs {
            self.program(&mut loc, program, &module, None);
        }
        diags
    }

    /// The records emitted so far, in order.
    pub fn records(&self) -> &[Rec] {
        &self.recs
    }

    /// Writes every record as one JSON line.
    pub fn write_jsonl<W: Write>(&self, w: &mut W) -> io::Result<()> {
        for rec in &self.recs {
            let line = serde_json::to_string(rec).expect("serialize record");
            writeln!(w, "{line}")?;
        }
        Ok(())
    }
}
