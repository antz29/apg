//! Static path rules shared by the resolvers: recognising a statically known
//! path argument, resolving it against the referencing file's directory, and
//! matching it case-insensitively against the scanned files.

use std::collections::HashMap;
use tree_sitter::Node;

/// A path-carrying argument, classified statically.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathArg {
    /// Literal path text; `anchored` when it was `$PSScriptRoot`-relative
    /// (the variable and its following separator already stripped).
    Static { text: String, anchored: bool },
    /// Anything whose value is only known at run time.
    Computed,
}

/// Classifies a command argument (incl. an invocation operator's `command`
/// field) or a hash-entry value, looking through single-child literal wrappers.
pub fn path_arg(node: Node, src: &str) -> PathArg {
    let node = unwrap(node);
    let text = |n: Node| n.utf8_text(src.as_bytes()).unwrap_or_default();
    let named: Vec<Node> = {
        let mut c = node.walk();
        node.named_children(&mut c).collect()
    };
    match node.kind() {
        "bare_word" => escaped_or(text(node), |s| fixed(s, false)),
        "single_quoted_string" => {
            let mut c = text(node).chars();
            c.next();
            c.next_back();
            fixed(&c.as_str().replace("''", "'"), false)
        }
        "expandable_string" => {
            let (anchored, rest) = match named.split_first() {
                Some((v, rest)) if is_script_root(v, src) => (true, rest),
                _ => (false, &named[..]),
            };
            if rest.iter().any(|n| n.kind() != "string_content") {
                return PathArg::Computed;
            }
            let s: String = rest.iter().map(|n| text(*n)).collect();
            escaped_or(&s, |s| {
                if anchored {
                    anchored_rest(s)
                } else {
                    fixed(s, false)
                }
            })
        }
        "command_argument" => match named.as_slice() {
            [v, f] if is_script_root(v, src) && f.kind() == "argument_fragment" => {
                escaped_or(text(*f), anchored_rest)
            }
            _ => PathArg::Computed,
        },
        _ => PathArg::Computed,
    }
}

/// Descends through the wrappers around a single literal value.
fn unwrap(mut node: Node) -> Node {
    while matches!(
        node.kind(),
        "pipeline" | "command_expression" | "command" | "command_name" | "command_argument"
    ) && node.named_child_count() == 1
    {
        node = node.named_child(0).expect("one named child");
    }
    node
}

fn is_script_root(node: &Node, src: &str) -> bool {
    node.kind() == "variable"
        && node.utf8_text(src.as_bytes()).is_ok_and(|t| {
            t.eq_ignore_ascii_case("$PSScriptRoot") || t.eq_ignore_ascii_case("${PSScriptRoot}")
        })
}

/// A backtick escape is an expansion too; its value is not the literal text.
fn escaped_or(s: &str, literal: impl FnOnce(&str) -> PathArg) -> PathArg {
    if s.contains('`') {
        PathArg::Computed
    } else {
        literal(s)
    }
}

fn fixed(text: &str, anchored: bool) -> PathArg {
    PathArg::Static {
        text: text.to_string(),
        anchored,
    }
}

/// The text after `$PSScriptRoot`, which must start with a separator.
fn anchored_rest(rest: &str) -> PathArg {
    match rest.strip_prefix(['\\', '/']) {
        Some(r) => fixed(r, true),
        None => PathArg::Computed,
    }
}

/// Drops empty and `.` segments and pops one per `..`; `None` when a `..`
/// climbs above the repository root.
pub fn normalise<'a>(segments: impl IntoIterator<Item = &'a str>) -> Option<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for s in segments {
        match s {
            "" | "." => {}
            ".." => {
                out.pop()?;
            }
            _ => out.push(s.to_string()),
        }
    }
    Some(out)
}

/// Resolves `text` (split on `\` and `/`) against the file's repo-relative
/// `/`-joined directory; `None` for empty text, absolute paths, or a climb
/// above the root.
pub fn resolve(file_dir: &str, text: &str) -> Option<String> {
    let b = text.as_bytes();
    let absolute = matches!(b.first(), None | Some(b'\\' | b'/' | b'~'))
        || (b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':');
    if absolute {
        return None;
    }
    let segs = file_dir.split('/').chain(text.split(['\\', '/']));
    Some(normalise(segs)?.join("/"))
}

/// The scanned repo-relative paths, matched case-insensitively.
pub struct FileIndex {
    by_lower: HashMap<String, Vec<String>>,
}

impl FileIndex {
    pub fn new(paths: impl IntoIterator<Item = String>) -> Self {
        let mut by_lower: HashMap<String, Vec<String>> = HashMap::new();
        for p in paths {
            by_lower.entry(p.to_lowercase()).or_default().push(p);
        }
        FileIndex { by_lower }
    }

    /// The unique case-insensitive match, else the exact-case one among
    /// several, else `None`.
    pub fn find(&self, path: &str) -> Option<&str> {
        match self.by_lower.get(&path.to_lowercase())?.as_slice() {
            [only] => Some(only),
            many => many.iter().find(|p| *p == path).map(String::as_str),
        }
    }
}
