//! Structural facts for the unified JSONL stream: a module per COBOL directory, a File per
//! source file, programs (and nested programs), sections, paragraphs and level-01 records as
//! Structs and Functions, with `contains` edges by scanner-local opaque id. Byte offsets are
//! exact over the origin file's text, so a declaration in a copybook indexes the copybook.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::io::{self, BufWriter, Write};
use std::path::{Component, Path, PathBuf};

use crate::ast::{Paragraph, ProcRange, Program, Section, Span, Statement, Transfer};
use crate::diag::{write_diagnostic, Diagnostic};
use crate::dialect::Dialect;
use crate::parser::parse_compilation_unit;
use crate::preprocess::{preprocess, read_file, Search};
use crate::resolve::{scopes, ProcKind, Resolution, Scope};
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
    /// A resolved control transfer: `properties.kind` is `perform` or `goto`.
    Calls {
        from: String,
        to: String,
        properties: BTreeMap<String, String>,
    },
    /// A `PERFORM` or `GO TO` naming a section: the section's Struct.
    Uses {
        from: String,
        to: String,
    },
    UnresolvedCall {
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
    unresolved: BTreeSet<String>,
    next_id: usize,
    base: PathBuf,
    prefix: String,
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
    scope: usize,
    ids: Vec<Vec<Option<String>>>,
    pending: Vec<Pending>,
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

impl Emitter {
    /// An emitter whose module identities are relative to `base` and whose ids start `prefix`.
    pub fn new(base: &Path, prefix: &str) -> Self {
        Emitter {
            recs: Vec::new(),
            modules: BTreeSet::new(),
            unresolved: BTreeSet::new(),
            next_id: 0,
            base: base.to_path_buf(),
            prefix: prefix.to_string(),
        }
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

    fn program(&mut self, loc: &mut Locate, p: &Program, parent: &str, up: Option<&str>) {
        loc.enter();
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
                self.paragraph(loc, para, &scope, &id, None);
            }
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
            to: id.clone(),
        });
        loc.claim(ProcKind::Paragraph, &para.name.text, section, &id);
        for sentence in &para.sentences {
            collect(&mut loc.pending, loc.scope, &id, &sentence.statements);
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
                        self.recs.push(Rec::Uses { from, to });
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
    }

    fn push_call(&mut self, from: String, to: String, kind: &str) {
        let properties = BTreeMap::from([("kind".to_string(), kind.to_string())]);
        self.recs.push(Rec::Calls {
            from,
            to,
            properties,
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
        let module = module_of(&self.base, Path::new(path));
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
            scopes: scopes(&unit),
            scope: 0,
            ids: Vec::new(),
            pending: Vec::new(),
            diags: Vec::new(),
        };
        for program in &unit.programs {
            self.program(&mut loc, program, &module, None);
        }
        self.transfer_facts(&mut loc);
        diags.extend(loc.diags);
        diags
    }

    /// The records emitted so far, in order.
    pub fn records(&self) -> &[Rec] {
        &self.recs
    }

    /// Keeps only the facts of the `targets` files (File records and the Struct/Function
    /// records located in them). A Module stays when a kept File sits in it; a `contains`
    /// edge stays when both ends do; a control-transfer edge stays when its source does, and
    /// an `unresolved` target when a kept `unresolved_call` names it.
    pub fn retain_targets(&mut self, targets: &HashSet<String>) {
        self.recs.retain(|r| match r {
            Rec::File { path, .. } | Rec::Struct { path, .. } | Rec::Function { path, .. } => {
                targets.contains(path)
            }
            Rec::Module { .. }
            | Rec::Contains { .. }
            | Rec::Calls { .. }
            | Rec::Uses { .. }
            | Rec::UnresolvedCall { .. }
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
        self.recs.retain(|r| match r {
            Rec::Module { fqn } => parents.contains(fqn),
            Rec::Contains { from, to } => ids.contains(from) && ids.contains(to),
            Rec::Calls { from, .. } | Rec::Uses { from, .. } | Rec::UnresolvedCall { from, .. } => {
                ids.contains(from)
            }
            _ => true,
        });
        let called: BTreeSet<String> = self
            .recs
            .iter()
            .filter_map(|r| match r {
                Rec::UnresolvedCall { to, .. } => Some(to.clone()),
                _ => None,
            })
            .collect();
        self.recs.retain(|r| match r {
            Rec::Unresolved { fqn, .. } => called.contains(fqn),
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

/// Queues the `PERFORM` (with a range) and `GO TO` names of `statements`, branches included,
/// from the paragraph `from` of scope `scope`. A `PERFORM` without a target is inline.
fn collect(out: &mut Vec<Pending>, scope: usize, from: &str, statements: &[Statement]) {
    for statement in statements {
        match &statement.transfer {
            Some(Transfer::Perform(Some(range))) => out.push(Pending {
                scope,
                from: from.to_string(),
                kind: "perform",
                range: range.clone(),
            }),
            Some(Transfer::GoTo(names)) => out.extend(names.iter().map(|name| Pending {
                scope,
                from: from.to_string(),
                kind: "goto",
                range: ProcRange {
                    from: name.clone(),
                    thru: None,
                },
            })),
            _ => {}
        }
        collect(out, scope, from, &statement.children);
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
        copy_paths: args.copy_paths.iter().map(PathBuf::from).collect(),
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

    if let Some(set) = &targets {
        emitter.retain_targets(set);
    }
    let mut out = BufWriter::new(io::stdout().lock());
    emitter.write_jsonl(&mut out).map_err(|e| e.to_string())?;
    out.flush().map_err(|e| e.to_string())
}
