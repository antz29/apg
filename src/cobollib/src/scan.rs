//! Structural facts for the unified JSONL stream: a module per COBOL directory, a File per
//! source file, programs (and nested programs), sections, paragraphs and level-01 records as
//! Structs and Functions, with `contains` edges by scanner-local opaque id. Byte offsets are
//! exact over the origin file's text, so a declaration in a copybook indexes the copybook.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::io::{self, BufWriter, Write};
use std::path::{Component, Path, PathBuf};

use crate::ast::{
    CallTarget, Class, DataEntry, Interface, Paragraph, ProcRange, Program, QualifiedName, Section,
    Span, Statement, Transfer,
};
use crate::diag::{write_diagnostic, Diagnostic};
use crate::dialect::Dialect;
use crate::lexer::TokenKind;
use crate::parser::parse_compilation_unit;
use crate::preprocess::{preprocess_with_copies, read_file, CopySite, Search};
use crate::resolve::{
    declared, declares, global_data, resolve_call, scopes, CallResolution, Declared, ProcKind,
    Resolution, Scope,
};
use crate::source_map::{FileId, Mapped, Origin, SourceMap};

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
        /// The overload key of a method (`factory`/`instance`); empty for paragraphs.
        #[serde(skip_serializing_if = "Vec::is_empty", default)]
        params: Vec<String>,
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
    /// A resolved control transfer: `properties.kind` is `perform` or `goto`.
    Calls {
        from: String,
        to: String,
        properties: BTreeMap<String, String>,
    },
    /// A `PERFORM` or `GO TO` naming a section, or a data reference to a record: the target's Struct.
    /// A `COPY` is a program's `uses` of the copybook's File, `properties.kind` `copy`.
    Uses {
        from: String,
        to: String,
        #[serde(skip_serializing_if = "BTreeMap::is_empty", default)]
        properties: BTreeMap<String, String>,
    },
    UnresolvedCall {
        from: String,
        to: String,
    },
    /// A data name a statement references that is neither a record nor declared in its program.
    UnresolvedUse {
        from: String,
        to: String,
    },
    Unresolved {
        fqn: String,
        category: String,
    },
}

/// Accumulates one scan's records. Ids are `<prefix>1`, `<prefix>2`, ... in emission order;
/// module identities are repo-relative to `base` (the scan directory).
#[derive(Debug)]
pub struct Emitter {
    recs: Vec<Rec>,
    modules: BTreeSet<String>,
    /// Every file that has its File record, whether walked or copied (one record each).
    files: BTreeSet<String>,
    unresolved: BTreeSet<String>,
    next_id: usize,
    base: PathBuf,
    prefix: String,
    /// Every emitted program, in emission order: its PROGRAM-ID and Struct id.
    programs: Vec<(String, String)>,
    /// Each scanned file's source map, indexed as [`PendingCall::file`].
    maps: Vec<SourceMap>,
    /// CALLs queued until [`Emitter::finish`] resolves them against the whole tree.
    calls: Vec<PendingCall>,
    /// The files holding an error-level diagnostic (the file a diagnostic is located in).
    errored: BTreeSet<String>,
}

/// A `CALL` awaiting whole-tree resolution: its calling paragraph's id, its target, and the
/// index of its file's map in `Emitter::maps`.
#[derive(Debug)]
struct PendingCall {
    from: String,
    target: CallTarget,
    file: usize,
}

/// A located span: the start origin's file name, byte offsets, and 1-based lines.
struct Loc {
    path: String,
    start: usize,
    end: usize,
    start_line: usize,
    end_line: usize,
}

/// A `PERFORM` or `GO TO` awaiting resolution: its enclosing paragraph's id, its program's
/// scope, the edge kind, and the procedure name or `from THRU thru` range it names.
struct Pending {
    scope: usize,
    from: String,
    kind: &'static str,
    range: ProcRange,
}

/// A data name a statement references, awaiting resolution against its program's records: its
/// enclosing paragraph's id, its program's scope index, and the name as written.
struct PendingRef {
    scope: usize,
    from: String,
    name: QualifiedName,
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

/// Locates spans by their origin file's text, reading copybook text through `lookup`. Also
/// carries the file's procedure scopes: `ids[s]` holds the emitted id of each procedure of
/// scope `s` (aligned to `scopes[s].procs`), `scope` the one being emitted.
struct Locate<'a> {
    map: &'a SourceMap,
    main: &'a str,
    main_text: &'a str,
    lookup: &'a dyn Fn(&Path) -> Option<String>,
    texts: BTreeMap<String, Text>,
    scopes: Vec<Scope>,
    /// Per scope (parallel to `scopes`), the non-data names it declares.
    decls: Vec<Declared>,
    scope: usize,
    ids: Vec<Vec<Option<String>>>,
    /// Each program and OO unit's span with its Struct id, pre-order, for [`enclosing`].
    owners: Vec<(Span, String)>,
    pending: Vec<Pending>,
    calls: Vec<PendingCall>,
    /// Per scope, the emitted id of each 01-level record by its upper-cased name (first kept).
    data_ids: Vec<BTreeMap<String, String>>,
    /// Per scope, every data name its DATA DIVISION declares at any level, upper-cased.
    declared: Vec<BTreeSet<String>>,
    data_refs: Vec<PendingRef>,
    /// This file's index in `Emitter::maps`.
    file: usize,
    diags: Vec<Diagnostic>,
}

impl Locate<'_> {
    /// Starts the next program in pre-order, whose scope index is the count so far.
    fn enter(&mut self) {
        self.scope = self.ids.len();
        self.ids
            .push(vec![None; self.scopes[self.scope].procs.len()]);
    }

    /// Records `id` as the procedure of the current scope matching `kind`, `name` and
    /// `section` (its owning section's index); returns its index in the scope's `procs`.
    fn claim(
        &mut self,
        kind: ProcKind,
        name: &str,
        section: Option<usize>,
        id: &str,
    ) -> Option<usize> {
        let procs = &self.scopes[self.scope].procs;
        let ids = &mut self.ids[self.scope];
        let at = (0..procs.len()).find(|&i| {
            ids[i].is_none()
                && procs[i].kind == kind
                && procs[i].name == name
                && procs[i].section == section
        })?;
        ids[at] = Some(id.to_string());
        Some(at)
    }

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

/// The module identity of `path`: its `/`-joined directory components relative to `base`,
/// EMPTY at the base itself (never `.`, never absolute).
fn module_of(base: &Path, path: &Path) -> String {
    let dir = path.parent().unwrap_or(path);
    dir.strip_prefix(base).map_or_else(
        |_| String::new(),
        |rel| {
            rel.components()
                .filter_map(|c| match c {
                    Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("/")
        },
    )
}

/// The innermost program or OO unit whose span holds `at`, a site in the main file. `owners` is
/// pre-order, so the last containing span is the innermost. A span end outside the main file (a
/// copybook's text) is open-ended: its line numbers belong to another file.
fn enclosing(owners: &[(Span, String)], map: &SourceMap, main: &str, at: Origin) -> Option<String> {
    let pos = (at.line, at.col);
    let key = |o: Origin| (map.name(o.file) == main).then_some((o.line, o.col));
    owners
        .iter()
        .rev()
        .find(|(span, _)| {
            key(span.start).unwrap_or((0, 0)) <= pos
                && pos <= key(span.end).unwrap_or((usize::MAX, 0))
        })
        .map(|(_, id)| id.clone())
}

impl Emitter {
    /// An emitter whose module identities are relative to `base` and whose ids start `prefix`.
    pub fn new(base: &Path, prefix: &str) -> Self {
        Emitter {
            recs: Vec::new(),
            modules: BTreeSet::new(),
            files: BTreeSet::new(),
            unresolved: BTreeSet::new(),
            next_id: 0,
            base: base.to_path_buf(),
            prefix: prefix.to_string(),
            programs: Vec::new(),
            maps: Vec::new(),
            calls: Vec::new(),
            errored: BTreeSet::new(),
        }
    }

    /// Records the files of `diags` that hold an error-level diagnostic.
    fn note_errors(&mut self, diags: &[Diagnostic]) {
        self.errored.extend(
            diags
                .iter()
                .filter(|d| !is_warning(&d.message))
                .map(|d| d.file.clone()),
        );
    }

    /// The closing stderr line: how many COBOL files had an error-level diagnostic.
    pub fn error_summary(&self) -> String {
        let n = self.errored.len();
        let files = if n == 1 { "file" } else { "files" };
        format!("cobol: {n} {files} had errors")
    }

    fn fresh_id(&mut self) -> String {
        self.next_id += 1;
        format!("{}{}", self.prefix, self.next_id)
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
        let scope = if parent.is_empty() {
            name.to_string()
        } else {
            format!("{parent}.{name}")
        };
        (id, scope)
    }

    /// A `CLASS-ID` Struct under `parent` (contained by `up` when nested) with a Function per
    /// method of its FACTORY and OBJECT sections, then `GET-`/`SET-` Functions per top-level
    /// PROPERTY item of their data divisions. Method bodies are not walked.
    fn class(&mut self, loc: &mut Locate, c: &Class, parent: &str, up: Option<&str>) {
        let (id, scope) = self.structure(loc, &c.name.text, c.span, parent, up);
        loc.owners.push((c.span, id.clone()));
        let mut seen = BTreeSet::new();
        for m in c.factory.iter().chain(&c.object).flat_map(|s| &s.methods) {
            // Factory and instance methods share a name only across the two keys below, so a
            // (kind, name) repeat is skipped (first kept) rather than colliding in the ingestor.
            if seen.insert((m.factory, m.name.text.clone())) {
                let key = if m.factory { "factory" } else { "instance" };
                self.member(
                    loc,
                    &m.name.text,
                    m.span,
                    &scope,
                    &id,
                    vec![key.to_string()],
                );
            }
        }
        // Properties are instance-keyed (static ones too), so they share the instance key with
        // methods: a same-named method keeps the slot and the property accessor is skipped.
        let entries = c
            .factory
            .iter()
            .chain(&c.object)
            .filter_map(|s| s.data.as_ref())
            .flat_map(|d| &d.sections)
            .flat_map(|s| &s.entries);
        for entry in entries {
            let Some(prop) = &entry.property else {
                continue;
            };
            let Some(name) = prop.as_name.as_ref().or(entry.name.as_ref()) else {
                continue;
            };
            let mut accessors = vec![format!("GET-{}", name.text)];
            if !prop.no_set {
                accessors.push(format!("SET-{}", name.text));
            }
            for accessor in accessors {
                if seen.insert((false, accessor.clone())) {
                    self.member(
                        loc,
                        &accessor,
                        entry.span,
                        &scope,
                        &id,
                        vec!["instance".to_string()],
                    );
                }
            }
        }
    }

    /// An `INTERFACE-ID` Struct with a Function per method prototype (a repeated name is kept once).
    fn interface(&mut self, loc: &mut Locate, i: &Interface, parent: &str, up: Option<&str>) {
        let (id, scope) = self.structure(loc, &i.name.text, i.span, parent, up);
        loc.owners.push((i.span, id.clone()));
        let mut seen = BTreeSet::new();
        for p in &i.prototypes {
            if seen.insert(p.name.text.clone()) {
                self.member(loc, &p.name.text, p.span, &scope, &id, Vec::new());
            }
        }
    }

    /// A Function named `name` located at `span`, contained by `up`; returns its id.
    fn member(
        &mut self,
        loc: &mut Locate,
        name: &str,
        span: Span,
        parent: &str,
        up: &str,
        params: Vec<String>,
    ) -> String {
        let l = loc.span(span);
        let id = self.fresh_id();
        self.recs.push(Rec::Function {
            id: id.clone(),
            parent: parent.to_string(),
            name: name.to_string(),
            params,
            file: l.path.clone(),
            path: l.path,
            start: l.start,
            end: l.end,
            start_line: l.start_line,
            end_line: l.end_line,
        });
        self.recs.push(Rec::Contains {
            from: up.to_string(),
            to: id.clone(),
        });
        id
    }

    fn program(&mut self, loc: &mut Locate, p: &Program, parent: &str, up: Option<&str>) {
        loc.enter();
        let (id, scope) = self.structure(loc, &p.name.text, p.span, parent, up);
        self.programs.push((p.name.text.clone(), id.clone()));
        loc.owners.push((p.span, id.clone()));
        let mut records = BTreeMap::new();
        let mut declared = BTreeSet::new();
        if let Some(data) = &p.data {
            for section in &data.sections {
                declared_names(&section.entries, &mut declared);
                for entry in &section.entries {
                    if let (1, Some(name)) = (entry.level, &entry.name) {
                        let (rec, _) =
                            self.structure(loc, &name.text, entry.span, &scope, Some(&id));
                        records.entry(name.text.to_ascii_uppercase()).or_insert(rec);
                    }
                }
            }
        }
        loc.data_ids.push(records);
        loc.declared.push(declared);
        if let Some(proc) = &p.procedure {
            for section in proc.declaratives.iter().chain(&proc.sections) {
                self.section(loc, section, &scope, &id);
            }
            for para in &proc.paragraphs {
                self.paragraph(loc, para, &scope, &id, None);
            }
        }
        for c in &p.classes {
            self.class(loc, c, &scope, Some(&id));
        }
        for nested in &p.nested {
            self.program(loc, nested, &scope, Some(&id));
        }
    }

    fn section(&mut self, loc: &mut Locate, s: &Section, parent: &str, up: &str) {
        let (id, scope) = self.structure(loc, &s.name.text, s.span, parent, Some(up));
        let at = loc.claim(ProcKind::Section, &s.name.text, None, &id);
        for para in &s.paragraphs {
            self.paragraph(loc, para, &scope, &id, at);
        }
    }

    fn paragraph(
        &mut self,
        loc: &mut Locate,
        para: &Paragraph,
        parent: &str,
        up: &str,
        section: Option<usize>,
    ) {
        let id = self.member(loc, &para.name.text, para.span, parent, up, Vec::new());
        loc.claim(ProcKind::Paragraph, &para.name.text, section, &id);
        for sentence in &para.sentences {
            collect(loc, &id, &sentence.statements);
        }
    }

    /// Resolves every pending `PERFORM`/`GO TO` of the file (all its programs are emitted by
    /// now, so forward references hold) into `calls`/`uses` edges, or `unresolved_call` with a
    /// diagnostic naming the kind and the location.
    fn transfer_facts(&mut self, loc: &mut Locate) {
        for p in std::mem::take(&mut loc.pending) {
            let scope = &loc.scopes[p.scope];
            let ids = &loc.ids[p.scope];
            let (from, kind) = (&p.from, p.kind);
            let resolved = match &p.range.thru {
                None => scope.resolve(&p.range.from),
                Some(thru) => scope.range(&p.range.from, thru),
            };
            let edge = |i: usize| ids[i].clone().map(|to| (from.clone(), to));
            let ambiguous = matches!(resolved, Resolution::Ambiguous);
            match resolved {
                Resolution::One(i) => {
                    let Some((from, to)) = edge(i) else { continue };
                    if scope.procs[i].kind == ProcKind::Section {
                        self.recs.push(Rec::Uses {
                            from,
                            to,
                            properties: BTreeMap::new(),
                        });
                    } else {
                        self.push_call(from, to, kind);
                    }
                }
                Resolution::Range(list) => {
                    for (from, to) in list.iter().filter_map(|&i| edge(i)) {
                        self.push_call(from, to, kind);
                    }
                }
                Resolution::Undefined | Resolution::Ambiguous => {
                    // The failing name: a bad `THRU` end only when the start resolved.
                    let failing = match &p.range.thru {
                        Some(thru)
                            if matches!(scope.resolve(&p.range.from), Resolution::One(_)) =>
                        {
                            thru
                        }
                        _ => &p.range.from,
                    };
                    let name = &failing.name;
                    let why = if ambiguous {
                        "names more than one procedure"
                    } else {
                        "names no procedure of its program"
                    };
                    loc.diags.push(
                        loc.map
                            .diagnostic(name.span.start, &format!("{kind} {} {why}", name.text)),
                    );
                    if self.unresolved.insert(name.text.clone()) {
                        self.recs.push(Rec::Unresolved {
                            fqn: name.text.clone(),
                            category: "unknown".to_string(),
                        });
                    }
                    self.recs.push(Rec::UnresolvedCall {
                        from: from.clone(),
                        to: name.text.clone(),
                    });
                }
            }
        }
        let mut linked = BTreeSet::new();
        let mut reported = BTreeSet::new();
        for r in std::mem::take(&mut loc.data_refs) {
            let scope = &loc.scopes[r.scope];
            let local = scope.data(&r.name).and_then(|i| {
                let key = scope.records[i].name.as_ref()?.text.to_ascii_uppercase();
                loc.data_ids[r.scope].get(&key)
            });
            let name = &r.name.name.text;
            let record = match local {
                Some(to) => Some(to.clone()),
                None if declares(&loc.scopes, &loc.decls, r.scope, &r.name)
                    || loc.declared[r.scope].contains(&name.to_ascii_uppercase()) =>
                {
                    continue
                }
                None => {
                    global_data(&loc.scopes, &loc.decls, r.scope, &r.name).and_then(|(a, i)| {
                        let key = loc.scopes[a].records[i]
                            .name
                            .as_ref()?
                            .text
                            .to_ascii_uppercase();
                        loc.data_ids[a].get(&key).cloned()
                    })
                }
            };
            match record {
                Some(to) => {
                    if linked.insert((r.from.clone(), to.clone())) {
                        self.recs.push(Rec::Uses {
                            from: r.from,
                            to,
                            properties: BTreeMap::new(),
                        });
                    }
                }
                None => {
                    if reported.insert((r.from.clone(), name.clone())) {
                        if self.unresolved.insert(name.clone()) {
                            self.recs.push(Rec::Unresolved {
                                fqn: name.clone(),
                                category: "unknown".to_string(),
                            });
                        }
                        self.recs.push(Rec::UnresolvedUse {
                            from: r.from,
                            to: name.clone(),
                        });
                    }
                }
            }
        }
    }

    fn push_call(&mut self, from: String, to: String, kind: &str) {
        let properties = BTreeMap::from([("kind".to_string(), kind.to_string())]);
        self.recs.push(Rec::Calls {
            from,
            to,
            properties,
        });
    }

    /// Emits a `copy` uses from the program holding each COPY to its copybook, deduplicated. A
    /// COPY in the main file belongs to the program enclosing it; a nested COPY (in a copybook)
    /// belongs to the program of the outermost COPY that pulled that copybook in. Sites are in
    /// expansion order, so the owner of a copybook is set before its nested COPYs are seen.
    fn copy_uses(
        &mut self,
        map: &SourceMap,
        main: &str,
        sites: &[CopySite],
        owners: &[(Span, String)],
    ) {
        let mut owner: BTreeMap<String, String> = BTreeMap::new();
        let mut linked = BTreeSet::new();
        for site in sites {
            let including = map.name(site.origin.file);
            let program = if including == main {
                enclosing(owners, map, main, site.origin)
            } else {
                owner.get(including).cloned()
            };
            let to = site.path.display().to_string();
            match program {
                Some(from) => {
                    owner.insert(to.clone(), from.clone());
                    if linked.insert((from.clone(), to.clone())) {
                        let properties = BTreeMap::from([("kind".to_string(), "copy".to_string())]);
                        self.recs.push(Rec::Uses {
                            from,
                            to,
                            properties,
                        });
                    }
                }
                None => {
                    owner.remove(&to);
                }
            }
        }
    }

    /// Resolves every queued `CALL` against the programs of the whole tree, so a call into
    /// another file holds. A resolved literal is a `uses` edge to its program's Struct; one
    /// naming no program or several (ambiguous) is an unresolved call, a data-name target is
    /// `func-value`. Returns the diagnostics (case-insensitive matches, ambiguity).
    pub fn finish(&mut self) -> Vec<Diagnostic> {
        let scopes: Vec<Scope> = self
            .programs
            .iter()
            .map(|(name, _)| Scope {
                name: name.clone(),
                parent: None,
                procs: Vec::new(),
                records: Vec::new(),
            })
            .collect();
        let mut diags = Vec::new();
        for call in std::mem::take(&mut self.calls) {
            let map = &self.maps[call.file];
            let (resolution, warning) = resolve_call(&scopes, map, &call.target);
            diags.extend(warning);
            let target = match &call.target {
                CallTarget::Literal(name) => name,
                CallTarget::Identifier(name) => &name.name,
            };
            let category = match resolution {
                CallResolution::Program(i) => {
                    let to = self.programs[i].1.clone();
                    self.recs.push(Rec::Uses {
                        from: call.from,
                        to,
                        properties: BTreeMap::new(),
                    });
                    continue;
                }
                CallResolution::Ambiguous => {
                    let message = format!("CALL {} names more than one program", target.text);
                    diags.push(map.diagnostic(target.span.start, &message));
                    "unknown"
                }
                CallResolution::External => "external",
                CallResolution::Dynamic => "func-value",
            };
            if self.unresolved.insert(target.text.clone()) {
                self.recs.push(Rec::Unresolved {
                    fqn: target.text.clone(),
                    category: category.to_string(),
                });
            }
            self.recs.push(Rec::UnresolvedCall {
                from: call.from,
                to: target.text.clone(),
            });
        }
        self.note_errors(&diags);
        diags
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
        let module = module_of(&self.base, Path::new(path));
        if self.files.insert(path.to_string()) {
            self.file_record(path, text);
        }
        // Every copybook the expansion reads, in read order, for its File record.
        let copied = RefCell::new(Vec::new());
        let read = |p: &Path| {
            let text = lookup(p)?;
            copied
                .borrow_mut()
                .push((p.display().to_string(), text.clone()));
            Some(text)
        };
        let mut map = SourceMap::default();
        let (tokens, mut diags, copies) =
            preprocess_with_copies(&mut map, path, text, dialect, search, &read);
        let (unit, more) = parse_compilation_unit(&map, dialect, &tokens);
        // A file with no IDENTIFICATION DIVISION (a copybook) has no standalone parse errors.
        if has_identification(&tokens) {
            diags.extend(more);
        }
        let file = self.maps.len();
        let mut loc = Locate {
            map: &map,
            main: path,
            main_text: text,
            lookup: &read,
            texts: BTreeMap::new(),
            scopes: scopes(&unit),
            decls: declared(&unit),
            scope: 0,
            ids: Vec::new(),
            owners: Vec::new(),
            pending: Vec::new(),
            calls: Vec::new(),
            data_ids: Vec::new(),
            declared: Vec::new(),
            data_refs: Vec::new(),
            file,
            diags: Vec::new(),
        };
        for program in &unit.programs {
            self.program(&mut loc, program, &module, None);
        }
        for c in &unit.classes {
            self.class(&mut loc, c, &module, None);
        }
        for i in &unit.interfaces {
            self.interface(&mut loc, i, &module, None);
        }
        self.transfer_facts(&mut loc);
        let owners = std::mem::take(&mut loc.owners);
        self.copy_uses(&map, path, &copies, &owners);
        diags.extend(loc.diags);
        self.calls.extend(loc.calls);
        self.maps.push(map);
        for (name, text) in copied.into_inner() {
            if self.files.insert(name.clone()) {
                self.file_record(&name, &text);
            }
        }
        self.note_errors(&diags);
        diags
    }

    /// Emits the Module (once) and the File record of `path`, whose text is `text`.
    fn file_record(&mut self, path: &str, text: &str) {
        let module = module_of(&self.base, Path::new(path));
        if self.modules.insert(module.clone()) {
            self.recs.push(Rec::Module {
                fqn: module.clone(),
            });
        }
        self.recs.push(Rec::File {
            path: path.to_string(),
            parent: module,
            start_line: 1,
            end_line: text.lines().count().max(1),
        });
    }

    /// The records emitted so far, in order.
    pub fn records(&self) -> &[Rec] {
        &self.recs
    }

    /// Keeps only the facts of the `targets` files (File records and the Struct/Function
    /// records located in them). A Module stays when a kept File sits in it; a `contains`
    /// edge stays when both ends do; a control-transfer or data `uses` edge stays when both
    /// ends do, and an unresolved-use edge when its source does; an `unresolved` target when a
    /// kept `unresolved_call` or `unresolved_use` names it. A control-transfer or data `uses`
    /// edge whose source stays but whose target declaration is dropped keeps its endpoint as
    /// that declaration's unrooted FQN (`parent.name`, the module identity verbatim).
    pub fn retain_targets(&mut self, targets: &HashSet<String>) {
        // Read before the first pass drops the non-target declarations.
        let fqns: BTreeMap<String, String> = self
            .recs
            .iter()
            .filter_map(|r| match r {
                Rec::Struct {
                    id, parent, name, ..
                }
                | Rec::Function {
                    id, parent, name, ..
                } => {
                    let fqn = if parent.is_empty() {
                        name.clone()
                    } else {
                        format!("{parent}.{name}")
                    };
                    Some((id.clone(), fqn))
                }
                _ => None,
            })
            .collect();
        self.recs.retain(|r| match r {
            Rec::File { path, .. } | Rec::Struct { path, .. } | Rec::Function { path, .. } => {
                targets.contains(path)
            }
            Rec::Module { .. }
            | Rec::Contains { .. }
            | Rec::Calls { .. }
            | Rec::Uses { .. }
            | Rec::UnresolvedCall { .. }
            | Rec::UnresolvedUse { .. }
            | Rec::Unresolved { .. } => true,
        });
        let ids: BTreeSet<String> = self
            .recs
            .iter()
            .filter_map(|r| match r {
                Rec::Struct { id, .. } | Rec::Function { id, .. } => Some(id.clone()),
                _ => None,
            })
            .collect();
        let parents: BTreeSet<String> = self
            .recs
            .iter()
            .filter_map(|r| match r {
                Rec::File { parent, .. } => Some(parent.clone()),
                _ => None,
            })
            .collect();
        for r in &mut self.recs {
            if let Rec::Calls { from, to, .. } | Rec::Uses { from, to, .. } = r {
                if ids.contains(from.as_str()) && !ids.contains(to.as_str()) {
                    if let Some(fqn) = fqns.get(to.as_str()) {
                        *to = fqn.clone();
                    }
                }
            }
        }
        self.recs.retain(|r| match r {
            Rec::Module { fqn } => parents.contains(fqn),
            Rec::Contains { from, to } => ids.contains(from) && ids.contains(to),
            Rec::Calls { from, .. } | Rec::Uses { from, .. } => ids.contains(from),
            Rec::UnresolvedCall { from, .. } | Rec::UnresolvedUse { from, .. } => {
                ids.contains(from)
            }
            _ => true,
        });
        let named: BTreeSet<String> = self
            .recs
            .iter()
            .filter_map(|r| match r {
                Rec::UnresolvedCall { to, .. } | Rec::UnresolvedUse { to, .. } => Some(to.clone()),
                _ => None,
            })
            .collect();
        self.recs.retain(|r| match r {
            Rec::Unresolved { fqn, .. } => named.contains(fqn),
            _ => true,
        });
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

const USAGE: &str = "usage: cobolfrontend <dir> [--module <dir>]... [--id-prefix <p>] \
     [--targets <file>] [--cache-dir <dir>] [--cache-key <key>] [-std=<dialect>] \
     [--copy-path <dir>]... [exclude...]";

/// The parsed command line (see [`run`]).
#[derive(Debug)]
pub(crate) struct Args {
    pub(crate) root: String,
    pub(crate) modules: Vec<String>,
    pub(crate) id_prefix: String,
    pub(crate) targets: Option<String>,
    pub(crate) dialect: Dialect,
    pub(crate) copy_paths: Vec<String>,
    pub(crate) excludes: Vec<String>,
}

/// Parses the argv after the program name. A value flag left without its value, and any
/// unknown argument, is an exclude glob (as the structural frontend treats them).
pub(crate) fn parse_args(argv: &[String]) -> Result<Args, String> {
    let Some((root, rest)) = argv.split_first() else {
        return Err(USAGE.to_string());
    };
    let mut args = Args {
        root: root.clone(),
        modules: Vec::new(),
        id_prefix: "n".to_string(),
        targets: None,
        dialect: Dialect::DEFAULT,
        copy_paths: Vec::new(),
        excludes: Vec::new(),
    };
    let mut i = 0;
    while i < rest.len() {
        let arg = rest[i].as_str();
        i += 1;
        let Some(value) = rest.get(i).filter(|_| value_flag(arg)).cloned() else {
            match arg.strip_prefix("-std=") {
                Some(name) => args.dialect = Dialect::from_name(name)?,
                None => args.excludes.push(arg.to_string()),
            }
            continue;
        };
        i += 1;
        match arg {
            "--module" => args.modules.push(value),
            "--id-prefix" => args.id_prefix = value,
            "--targets" => args.targets = Some(value),
            "--copy-path" => args.copy_paths.push(value),
            // Accepted for the pinned hand-off, then ignored (no native cache).
            _ => {}
        }
    }
    Ok(args)
}

/// True for the flags that consume the following argument.
fn value_flag(arg: &str) -> bool {
    matches!(
        arg,
        "--module" | "--id-prefix" | "--targets" | "--copy-path" | "--cache-dir" | "--cache-key"
    )
}

/// Queues the `PERFORM`, `SORT`/`MERGE` procedure and `GO TO` names of `statements`, branches
/// included, from the paragraph `from` of the current scope. A `PERFORM` without a target is
/// inline; `RESUME AT` queues kind `resume`; `RESUME NEXT STATEMENT` has no transfer. A `CALL`
/// is queued as a [`PendingCall`]; each operand data name as a [`PendingRef`].
fn collect(loc: &mut Locate, from: &str, statements: &[Statement]) {
    let scope = loc.scope;
    for statement in statements {
        loc.data_refs
            .extend(statement.refs.iter().map(|name| PendingRef {
                scope,
                from: from.to_string(),
                name: name.clone(),
            }));
        match &statement.transfer {
            Some(Transfer::Perform(Some(range))) => loc.pending.push(Pending {
                scope,
                from: from.to_string(),
                kind: "perform",
                range: range.clone(),
            }),
            Some(Transfer::SortMerge { input, output }) => {
                loc.pending
                    .extend(input.iter().chain(output).map(|range| Pending {
                        scope,
                        from: from.to_string(),
                        kind: "perform",
                        range: range.clone(),
                    }));
            }
            Some(Transfer::Resume(name)) => loc.pending.push(Pending {
                scope,
                from: from.to_string(),
                kind: "resume",
                range: ProcRange {
                    from: name.clone(),
                    thru: None,
                },
            }),
            Some(Transfer::GoTo(names)) => loc.pending.extend(names.iter().map(|name| Pending {
                scope,
                from: from.to_string(),
                kind: "goto",
                range: ProcRange {
                    from: name.clone(),
                    thru: None,
                },
            })),
            Some(Transfer::Call(target)) => loc.calls.push(PendingCall {
                from: from.to_string(),
                target: target.clone(),
                file: loc.file,
            }),
            _ => {}
        }
        collect(loc, from, &statement.children);
    }
}

/// `path` made absolute and symlink-resolved when it exists (so the walk and the target list
/// agree on identities).
fn absolutize(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir().map_or_else(|_| path.to_path_buf(), |d| d.join(path))
        }
    })
}

/// Adds the upper-cased name of each entry in `entries`, and beneath it, to `out`.
fn declared_names(entries: &[DataEntry], out: &mut BTreeSet<String>) {
    for entry in entries {
        if let Some(name) = &entry.name {
            out.insert(name.text.to_ascii_uppercase());
        }
        declared_names(&entry.children, out);
    }
}

/// Whether the expanded tokens hold an `IDENTIFICATION DIVISION` header (`ID DIVISION` too).
fn has_identification(tokens: &[Mapped]) -> bool {
    tokens.windows(2).any(|w| {
        matches!(w[0].token.kind, TokenKind::Word | TokenKind::Reserved)
            && matches!(
                w[0].token.text.to_ascii_uppercase().as_str(),
                "IDENTIFICATION" | "ID"
            )
            && w[1].token.text.eq_ignore_ascii_case("DIVISION")
    })
}

/// Whether a diagnostic is a warning rather than an error, by its wording: the free-format
/// heuristic, a feature level below error (`warning`, `archaic`, `obsolete`), and a
/// case-insensitive `CALL` match.
fn is_warning(message: &str) -> bool {
    message.starts_with("warning:")
        || [
            " is Warning under this dialect",
            " is Archaic under this dialect",
            " is Obsolete under this dialect",
            "matches a PROGRAM-ID only case-insensitively",
        ]
        .iter()
        .any(|end| message.ends_with(end))
}

/// The COBOL source extensions, in any letter case.
fn is_cobol_source(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        matches!(
            e.to_ascii_lowercase().as_str(),
            "cob" | "cbl" | "cobol" | "cpy"
        )
    })
}

/// Every claimed source at or below `dir`. Build, VCS, worktree and the top-level `apg/`
/// layout trees are pruned, as in the structural frontend.
fn walk(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(ft) = entry.file_type() else { continue };
        let path = entry.path();
        if ft.is_dir() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let pruned = matches!(
                name.as_str(),
                "target" | "node_modules" | ".git" | ".worktrees"
            );
            if pruned || (dir == root && name == "apg") {
                continue;
            }
            walk(root, &path, out);
        } else if ft.is_file() && is_cobol_source(&path) {
            out.push(path);
        }
    }
}

/// The `--targets` list: absolute paths, one per line, blanks ignored. An absent, unreadable
/// or empty list is no filter (the full scan).
fn read_targets(path: Option<&str>) -> Option<HashSet<String>> {
    let path = path?;
    let Ok(text) = std::fs::read_to_string(path) else {
        eprintln!("warning: could not read targets {path}; scanning unfiltered");
        return None;
    };
    let set: HashSet<String> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| absolutize(Path::new(l)).to_string_lossy().into_owned())
        .collect();
    (!set.is_empty()).then_some(set)
}

/// Runs the COBOL frontend over `argv` (the process arguments after the program name): walks
/// the tree under the positional directory, parses every claimed file (the whole tree is
/// resolved so cross-file references hold), writes each file's diagnostics to stderr, then
/// writes the facts to stdout, filtered to the `--targets` files when given.
pub fn run(argv: &[String]) -> Result<(), String> {
    let args = parse_args(argv)?;
    let root = absolutize(Path::new(&args.root));
    let module_dirs: Vec<PathBuf> = args
        .modules
        .iter()
        .map(|m| absolutize(&root.join(m)))
        .collect();
    let targets = read_targets(args.targets.as_deref());
    let search = Search {
        scan_root: root.clone(),
        copy_paths: args
            .copy_paths
            .iter()
            .map(|p| absolutize(Path::new(p)))
            .collect(),
    };

    let mut files = Vec::new();
    walk(&root, &root, &mut files);
    files.retain(|p| {
        let s = p.to_string_lossy();
        (module_dirs.is_empty() || module_dirs.iter().any(|m| p.starts_with(m)))
            && !args.excludes.iter().any(|x| s.contains(x.as_str()))
    });
    files.sort();

    let mut emitter = Emitter::new(&root, &args.id_prefix);
    let mut err = io::stderr();
    for path in &files {
        let Ok(bytes) = std::fs::read(path) else {
            eprintln!("warning: could not read {}", path.display());
            continue;
        };
        let text = String::from_utf8_lossy(&bytes);
        let diags = emitter.scan_file(
            &path.to_string_lossy(),
            &text,
            args.dialect,
            &search,
            &read_file,
        );
        for diag in &diags {
            write_diagnostic(&mut err, diag).map_err(|e| e.to_string())?;
        }
    }

    for diag in emitter.finish() {
        write_diagnostic(&mut err, &diag).map_err(|e| e.to_string())?;
    }
    writeln!(err, "{}", emitter.error_summary()).map_err(|e| e.to_string())?;
    if let Some(set) = &targets {
        emitter.retain_targets(set);
    }
    let mut out = BufWriter::new(io::stdout().lock());
    emitter.write_jsonl(&mut out).map_err(|e| e.to_string())?;
    out.flush().map_err(|e| e.to_string())
}
