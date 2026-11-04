//! End-to-end parse of the vendored NIST CCVS85 corpus (`tests/nist/`). Every program whose
//! file stem starts with NC, IC, IF, IX, OB, RL, SG, SM, SQ or ST is read in place and parsed
//! through `parse_source` under `cobol85`: no error-level diagnostic may remain, and the OB and
//! SG groups must each give at least one obsolete-level warning. Run via
//! `cargo test e2e:: -- --ignored --test-threads=4`.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use cobolfrontend::ast::{
    CompilationUnit, DataEntry, ProcedureDivision, Program, QualifiedName, Statement,
};
use cobolfrontend::diag::Diagnostic;
use cobolfrontend::dialect::Dialect;
use cobolfrontend::lexer::TokenKind;
use cobolfrontend::parser::{parse_compilation_unit, parse_source};
use cobolfrontend::preprocess::{preprocess, read_file, Search};
use cobolfrontend::source_map::{Mapped, SourceMap};

/// Figurative constants and special registers: data names a statement may use without a
/// DATA DIVISION entry.
const FIGURATIVE: &[&str] = &[
    "ZERO",
    "ZEROS",
    "ZEROES",
    "SPACE",
    "SPACES",
    "HIGH-VALUE",
    "HIGH-VALUES",
    "LOW-VALUE",
    "LOW-VALUES",
    "QUOTE",
    "QUOTES",
    "NULL",
    "NULLS",
    "ALL",
    "RETURN-CODE",
    "SORT-RETURN",
    "SORT-CORE-SIZE",
    "SORT-FILE-SIZE",
    "SORT-MODE-SIZE",
    "SORT-MESSAGE",
    "TALLY",
    "LINAGE-COUNTER",
    "LINE-COUNTER",
    "PAGE-COUNTER",
    "DEBUG-ITEM",
    "DEBUG-LINE",
    "DEBUG-NAME",
    "DEBUG-CONTENTS",
    "DEBUG-SUB-1",
    "DEBUG-SUB-2",
    "DEBUG-SUB-3",
];

const PREFIXES: [&str; 10] = ["NC", "IC", "IF", "IX", "OB", "RL", "SG", "SM", "SQ", "ST"];

/// The obsolete-level warning is the only diagnostic that names its level, in its wording
/// (`parser/procedure.rs`, `feature`); every other diagnostic counts as error-level.
fn is_obsolete(d: &Diagnostic) -> bool {
    d.message.ends_with(" is Obsolete under this dialect")
}

/// Whether `path` is a corpus program: a `.CBL`/`.SUB` file (any case) whose stem starts with
/// an in-scope prefix (case-insensitive). `*.log`, `*.DAT` and copybooks never match.
fn in_scope(path: &Path) -> bool {
    let ext_ok = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("CBL") || e.eq_ignore_ascii_case("SUB"));
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_uppercase();
    ext_ok && PREFIXES.iter().any(|p| stem.starts_with(p))
}

/// Recursively collects the in-scope programs under `dir` (a module folder and its `lib/`).
fn walk(dir: &Path, programs: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("NIST corpus missing at {}: {e}", dir.display()));
    for entry in entries {
        let path = entry.expect("read corpus entry").path();
        if path.is_dir() {
            walk(&path, programs);
        } else if in_scope(&path) {
            programs.push(path);
        }
    }
}

/// The in-scope programs of the ten module folders under `root`, sorted. The copybook folders
/// (`copy/`, `copyalt/`) sit beside the module folders and are never walked. Panics naming
/// `root` when no program is found.
fn corpus(root: &Path) -> Vec<PathBuf> {
    let mut programs = Vec::new();
    for module in PREFIXES {
        walk(&root.join(module), &mut programs);
    }
    programs.sort();
    assert!(
        !programs.is_empty(),
        "no in-scope NIST program (NC/IC/IF/IX/OB/RL/SG/SM/SQ/ST) under {}",
        root.display()
    );
    programs
}

/// The corpus group of a program: its matching prefix.
fn group(path: &Path) -> &'static str {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_uppercase();
    PREFIXES
        .iter()
        .find(|p| stem.starts_with(*p))
        .copied()
        .unwrap_or("")
}

/// Parses one program through the in-memory reader, with the corpus dir as the scan root.
fn parse_program(path: &Path, dir: &Path) -> Vec<Diagnostic> {
    let text = read_file(path).unwrap_or_else(|| panic!("cannot read {}", path.display()));
    let search = Search {
        scan_root: dir.to_path_buf(),
        copy_paths: Vec::new(),
    };
    let (_, diags) = parse_source(
        &path.display().to_string(),
        &text,
        Dialect::Cobol85,
        &search,
        &read_file,
    );
    diags
}

/// Groups a diagnostic message into one rule: backtick, double- and single-quoted spans become
/// `` `_` ``/`"_"`/`'_'` and each digit run becomes `N`, so `` `WS-A1` `` and `` `WS-B2` ``
/// share one rule.
fn normalise(message: &str) -> String {
    let mut out = String::new();
    let mut chars = message.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '`' | '"' | '\'' => {
                for d in chars.by_ref() {
                    if d == c {
                        break;
                    }
                }
                out.extend([c, '_', c]);
            }
            '0'..='9' => {
                while chars.peek().is_some_and(char::is_ascii_digit) {
                    chars.next();
                }
                out.push('N');
            }
            _ => out.push(c),
        }
    }
    out
}

/// Parses one program keeping its token stream, as `parse_source` does (COPY/REPLACE expansion,
/// then the parser) but returning the tokens the data-reference check reads.
fn parse_with_tokens(path: &Path, dir: &Path) -> (SourceMap, Vec<Mapped>, CompilationUnit) {
    let text = read_file(path).unwrap_or_else(|| panic!("cannot read {}", path.display()));
    let search = Search {
        scan_root: dir.to_path_buf(),
        copy_paths: Vec::new(),
    };
    let mut map = SourceMap::default();
    let (tokens, _) = preprocess(
        &mut map,
        &path.display().to_string(),
        &text,
        Dialect::Cobol85,
        &search,
        &read_file,
    );
    let (unit, _) = parse_compilation_unit(&map, Dialect::Cobol85, &tokens);
    (map, tokens, unit)
}

/// Every program in `programs`, nested ones included, in source order.
fn nested_programs<'a>(programs: &'a [Program], out: &mut Vec<&'a Program>) {
    for program in programs {
        out.push(program);
        nested_programs(&program.nested, out);
    }
}

/// The tokens of `program`, from its first token to its last (nested programs included).
fn program_tokens<'a>(tokens: &'a [Mapped], program: &Program) -> &'a [Mapped] {
    let start = tokens
        .iter()
        .position(|t| t.origin == program.span.start)
        .unwrap_or(0);
    let end = tokens[start..]
        .iter()
        .position(|t| t.origin == program.span.end)
        .map_or(tokens.len(), |k| start + k + 1);
    &tokens[start..end]
}

/// Adds the data names of `entries` and their children (level-88 condition names included).
fn declare_entries(entries: &[DataEntry], out: &mut HashSet<String>) {
    for entry in entries {
        if let Some(name) = &entry.name {
            out.insert(name.text.to_ascii_uppercase());
        }
        declare_entries(&entry.children, out);
    }
}

/// The word at `t` when it is a plain word, added as declared in upper case.
fn declare_word(t: Option<&Mapped>, out: &mut HashSet<String>) {
    if let Some(t) = t.filter(|t| t.token.kind == TokenKind::Word) {
        out.insert(t.token.text.to_ascii_uppercase());
    }
}

/// Names a program declares in its token stream outside the DATA DIVISION: file names after
/// SELECT, FD and SD, index names after INDEXED BY, and the mnemonic (after IS) and alphabet
/// (after ALPHABET) names of SPECIAL-NAMES.
fn token_declared(tokens: &[Mapped], out: &mut HashSet<String>) {
    let word = |t: &Mapped| matches!(t.token.kind, TokenKind::Word | TokenKind::Reserved);
    let mut in_special = false;
    for (i, t) in tokens.iter().enumerate() {
        if !word(t) {
            continue;
        }
        match t.token.text.to_ascii_uppercase().as_str() {
            "SELECT" | "FD" | "SD" => {
                let next = tokens.get(i + 1);
                let optional = next.is_some_and(|n| n.token.text.eq_ignore_ascii_case("OPTIONAL"));
                declare_word(tokens.get(i + 1 + usize::from(optional)), out);
            }
            "BY" if i > 0 && tokens[i - 1].token.text.eq_ignore_ascii_case("INDEXED") => {
                for n in tokens[i + 1..]
                    .iter()
                    .take_while(|n| matches!(n.token.kind, TokenKind::Word | TokenKind::Comma))
                {
                    declare_word(Some(n), out);
                }
            }
            "SPECIAL-NAMES" => in_special = true,
            "FILE-CONTROL" | "I-O-CONTROL" | "DATA" | "PROCEDURE" => in_special = false,
            "IS" if in_special => declare_word(tokens.get(i + 1), out),
            "ALPHABET" if in_special => declare_word(tokens.get(i + 1), out),
            _ => {}
        }
    }
}

/// Every data-name reference a statement makes, nested statements included.
fn statement_refs<'a>(statements: &'a [Statement], out: &mut Vec<&'a QualifiedName>) {
    for statement in statements {
        out.extend(statement.refs.iter());
        statement_refs(&statement.children, out);
    }
}

/// Every data-name reference in a procedure division: declaratives, sections, paragraphs.
fn procedure_refs(division: &ProcedureDivision) -> Vec<&QualifiedName> {
    let mut refs = Vec::new();
    let sections = division
        .declaratives
        .iter()
        .chain(&division.sections)
        .flat_map(|s| &s.paragraphs);
    for paragraph in division.paragraphs.iter().chain(sections) {
        for sentence in &paragraph.sentences {
            statement_refs(&sentence.statements, &mut refs);
        }
    }
    refs
}

mod e2e {
    use super::*;

    #[test]
    #[ignore = "e2e tier: reads the vendored NIST CCVS85 corpus; run via cargo test e2e:: -- --ignored"]
    fn nist_data_references_are_declared() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/nist");
        let mut checked = 0;
        let mut bad_programs = 0;
        let mut total = 0;
        let mut groups: BTreeMap<String, (usize, String)> = BTreeMap::new();
        for path in &corpus(&dir) {
            let (map, tokens, unit) = parse_with_tokens(path, &dir);
            let mut all = Vec::new();
            nested_programs(&unit.programs, &mut all);
            for program in all {
                checked += 1;
                let mut declared = HashSet::new();
                if let Some(data) = &program.data {
                    for section in &data.sections {
                        declare_entries(&section.entries, &mut declared);
                    }
                }
                token_declared(program_tokens(&tokens, program), &mut declared);
                let refs = program
                    .procedure
                    .as_ref()
                    .map(procedure_refs)
                    .unwrap_or_default();
                let mut program_bad = false;
                for reference in refs {
                    for n in std::iter::once(&reference.name).chain(&reference.qualifiers) {
                        let key = n.text.to_ascii_uppercase();
                        if declared.contains(&key) || FIGURATIVE.contains(&key.as_str()) {
                            continue;
                        }
                        total += 1;
                        program_bad = true;
                        let at = n.span.start;
                        let example = format!("{}:{}:{}", map.name(at.file), at.line, at.col);
                        groups.entry(key).or_insert((0, example)).0 += 1;
                    }
                }
                bad_programs += usize::from(program_bad);
            }
        }
        let undeclared = groups.len();
        let mut ranked: Vec<(String, (usize, String))> = groups.into_iter().collect();
        ranked.sort_by(|a, b| b.1 .0.cmp(&a.1 .0).then_with(|| a.0.cmp(&b.0)));
        let mut report = vec![
            format!(
                "NIST data-reference summary: {checked} programs checked, {bad_programs} with undeclared refs, {total} undeclared refs"
            ),
            format!("undeclared names: {undeclared} groups (count, name, e.g. file:line:col)"),
        ];
        report.extend(
            ranked
                .iter()
                .take(150)
                .map(|(name, (n, example))| format!("  {n:>5}  {name}  e.g. {example}")),
        );
        if undeclared > 150 {
            report.push(format!("... {} more groups not shown", undeclared - 150));
        }
        assert!(total == 0, "{}", report.join("\n"));
    }

    #[test]
    #[ignore = "e2e tier: reads the vendored NIST CCVS85 corpus; run via cargo test e2e:: -- --ignored"]
    fn nist_programs_parse_without_errors_and_obsolete_groups_warn() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/nist");
        let programs = corpus(&dir);
        let mut failures = Vec::new();
        let mut obsolete: BTreeMap<&str, usize> = BTreeMap::new();
        let mut per_module: BTreeMap<&str, usize> = BTreeMap::new();
        let mut rules: BTreeMap<String, (usize, String)> = BTreeMap::new();
        let mut total_errors = 0;
        for path in &programs {
            *per_module.entry(group(path)).or_default() += 1;
            let diags = parse_program(path, &dir);
            let (warnings, errors): (Vec<_>, Vec<_>) = diags.iter().partition(|d| is_obsolete(d));
            *obsolete.entry(group(path)).or_default() += warnings.len();
            if !errors.is_empty() {
                total_errors += errors.len();
                for d in &errors {
                    let example = format!("{}:{}:{}", d.file, d.line, d.col);
                    rules.entry(normalise(&d.message)).or_insert((0, example)).0 += 1;
                }
                let first: Vec<String> = errors.iter().take(3).map(|d| d.to_string()).collect();
                failures.push(format!(
                    "{} ({} error-level):\n  {}",
                    path.display(),
                    errors.len(),
                    first.join("\n  ")
                ));
            }
        }
        let mut top: Vec<(String, (usize, String))> = rules.into_iter().collect();
        top.sort_by(|a, b| b.1 .0.cmp(&a.1 .0).then_with(|| a.0.cmp(&b.0)));
        let modules: Vec<String> = per_module.iter().map(|(m, n)| format!("{m}={n}")).collect();
        let mut report = vec![
            format!("NIST summary: {} programs parsed", programs.len()),
            format!("per module: {}", modules.join(" ")),
            format!(
                "programs with errors: {} of {}; error-level diagnostics: {total_errors}",
                failures.len(),
                programs.len()
            ),
            "top 25 messages (count, normalised rule, example file:line:col):".to_string(),
        ];
        report.extend(
            top.iter()
                .take(25)
                .map(|(rule, (n, example))| format!("  {n:>5}  {rule}  e.g. {example}")),
        );
        report.push(format!(
            "--- per-program detail (first 40 of {} failing programs, 3 diagnostics each) ---",
            failures.len()
        ));
        report.extend(failures.iter().take(40).cloned());
        if failures.len() > 40 {
            report.push(format!(
                "... {} more failing programs not shown",
                failures.len() - 40
            ));
        }
        assert!(failures.is_empty(), "{}", report.join("\n"));
        for name in ["OB", "SG"] {
            assert!(
                obsolete.get(name).copied().unwrap_or(0) > 0,
                "no obsolete-level warning in the {name} group; obsolete counts: {obsolete:?}"
            );
        }
    }
}
