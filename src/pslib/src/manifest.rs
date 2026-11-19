//! Module manifests (`.psd1`): recognising one, reading its RootModule,
//! NestedModules and FunctionsToExport keys, and matching exported names.

use crate::paths::{path_arg, PathArg};
use tree_sitter::{Node, Tree};

/// Which functions a manifest exports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportSpec {
    /// No FunctionsToExport key: every function is exported.
    All,
    /// The literal FunctionsToExport patterns.
    Patterns(Vec<String>),
}

impl ExportSpec {
    pub fn matches(&self, name: &str) -> bool {
        match self {
            ExportSpec::All => true,
            ExportSpec::Patterns(ps) => ps.iter().any(|p| wildcard_match(p, name)),
        }
    }

    /// The matching names, in input order.
    pub fn exported<'a>(&self, functions: impl IntoIterator<Item = &'a str>) -> Vec<&'a str> {
        functions.into_iter().filter(|f| self.matches(f)).collect()
    }
}

enum Tok {
    Star,
    One,
    Set(Vec<(char, char)>),
    Lit(char),
}

fn tokens(pattern: &str) -> Vec<Tok> {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < p.len() {
        match p[i] {
            '*' => out.push(Tok::Star),
            '?' => out.push(Tok::One),
            '`' if i + 1 < p.len() => {
                i += 1;
                out.push(Tok::Lit(p[i]));
            }
            '[' => match p[i + 1..].iter().position(|&c| c == ']') {
                Some(len) => {
                    let body = &p[i + 1..i + 1 + len];
                    let mut set = Vec::new();
                    let mut j = 0;
                    while j < body.len() {
                        if j + 2 < body.len() && body[j + 1] == '-' {
                            set.push((body[j], body[j + 2]));
                            j += 3;
                        } else {
                            set.push((body[j], body[j]));
                            j += 1;
                        }
                    }
                    out.push(Tok::Set(set));
                    i += len + 1;
                }
                // An unclosed `[` is taken literally.
                None => out.push(Tok::Lit('[')),
            },
            c => out.push(Tok::Lit(c)),
        }
        i += 1;
    }
    out
}

/// PowerShell wildcard matching, case-insensitive: `*`, `?`, `[abc]`/`[a-z]`
/// and a backtick escaping the next character.
pub fn wildcard_match(pattern: &str, name: &str) -> bool {
    let toks = tokens(pattern);
    let s: Vec<char> = name.to_lowercase().chars().collect();
    let one = |t: &Tok, c: char| match t {
        Tok::One => true,
        Tok::Lit(l) => *l == c,
        Tok::Set(set) => set.iter().any(|&(a, b)| a <= c && c <= b),
        Tok::Star => false,
    };
    let (mut t, mut i) = (0, 0);
    let mut back: Option<(usize, usize)> = None;
    while i < s.len() {
        if t < toks.len() && matches!(toks[t], Tok::Star) {
            back = Some((t, i));
            t += 1;
        } else if t < toks.len() && one(&toks[t], s[i]) {
            t += 1;
            i += 1;
        } else if let Some((bt, bi)) = back {
            t = bt + 1;
            i = bi + 1;
            back = Some((bt, bi + 1));
        } else {
            return false;
        }
    }
    toks[t..].iter().all(|k| matches!(k, Tok::Star))
}

/// The keys a manifest carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestKeys {
    pub root_module: Option<PathArg>,
    pub nested_modules: Vec<PathArg>,
    pub exports: ExportSpec,
}

/// Reads the file's top-level hashtable; `None` unless it holds a
/// RootModule, NestedModules or FunctionsToExport key.
pub fn read_manifest(src: &str, tree: &Tree) -> Option<ManifestKeys> {
    let root = tree.root_node();
    if root.kind() != "source_file" {
        return None;
    }
    let mut cur = root.walk();
    let table = root
        .named_children(&mut cur)
        .find_map(|n| Some(unwrap(n)).filter(|n| n.kind() == "hashtable_expression"))?;
    let (mut root_module, mut nested, mut exports) = (None, None, None);
    let mut cur = table.walk();
    // ERROR children are skipped: only well-formed direct entries are read.
    for entry in table
        .named_children(&mut cur)
        .filter(|e| e.kind() == "hash_entry")
    {
        let (Some(k), Some(v)) = (
            entry.child_by_field_name("key"),
            entry.child_by_field_name("value"),
        ) else {
            continue;
        };
        let key = key_text(k, src).to_ascii_lowercase();
        // A duplicate key is a PowerShell error; the first one wins.
        match key.as_str() {
            "rootmodule" if root_module.is_none() => root_module = Some(path_arg(v, src)),
            "nestedmodules" if nested.is_none() => {
                nested = Some(elements(v).into_iter().map(|n| path_arg(n, src)).collect())
            }
            "functionstoexport" if exports.is_none() => {
                exports = Some(ExportSpec::Patterns(
                    elements(v)
                        .into_iter()
                        .filter_map(|n| match path_arg(n, src) {
                            PathArg::Static {
                                text,
                                anchored: false,
                            } => Some(text),
                            _ => None,
                        })
                        .collect(),
                ))
            }
            _ => {}
        }
    }
    if root_module.is_none() && nested.is_none() && exports.is_none() {
        return None;
    }
    Some(ManifestKeys {
        root_module,
        nested_modules: nested.unwrap_or_default(),
        exports: exports.unwrap_or(ExportSpec::All),
    })
}

/// Descends through single-child `pipeline`/`command_expression` wrappers.
fn unwrap(mut n: Node) -> Node {
    while matches!(n.kind(), "pipeline" | "command_expression") && n.named_child_count() == 1 {
        n = n.named_child(0).expect("one named child");
    }
    n
}

/// A key's text: an identifier, or a quoted string with its quotes removed.
fn key_text<'s>(k: Node, src: &'s str) -> &'s str {
    let t = &src[k.byte_range()];
    if k.kind().ends_with("string") && t.len() >= 2 {
        &t[1..t.len() - 1]
    } else {
        t
    }
}

/// The elements of a value: a single value, an `@( )` array or a comma list.
fn elements(v: Node) -> Vec<Node> {
    let n = unwrap(v);
    match n.kind() {
        "array_expression" | "array_literal_expression" => {
            let mut cur = n.walk();
            n.named_children(&mut cur)
                .filter(|c| c.kind() != "comment")
                .flat_map(elements)
                .collect()
        }
        _ => vec![n],
    }
}
