//! Live `apg scan` progress on stderr: the start line, one kind symbol per
//! node as it enters the graph, and the closing per-language/per-kind summary
//! carrying the symbol legend inline.
//!
//! Progress reaches stderr alone (never `apg-frontend.log`), and the whole
//! stream is gated on stderr being a terminal and the run carrying no
//! `--no-progress` flag.

use std::io::Write;

use crate::graph::NodeKind;

/// The five code kinds that stream progress — `(symbol, kind label)` — and the
/// inline legend the closing summary carries. Only these kinds emit: authored
/// layer, plan, and feedback nodes never do.
pub const KINDS: [(&str, &str); 5] = [
    ("🌍", "Language"),
    ("📦", "Module"),
    ("📄", "File"),
    ("🧱", "Struct"),
    ("⚙️", "Function"),
];

/// The [`KINDS`] position of a code node kind, or `None` for a kind that never
/// streams (authored layer, plan, and feedback nodes).
fn kind_index(kind: NodeKind) -> Option<usize> {
    Some(match kind {
        NodeKind::Language => 0,
        NodeKind::Module => 1,
        NodeKind::File => 2,
        NodeKind::Struct => 3,
        NodeKind::Function => 4,
        _ => return None,
    })
}

/// The symbol [`KINDS`] pairs with a code node kind, or `None` for a kind that
/// never streams.
pub fn symbol_for(kind: NodeKind) -> Option<&'static str> {
    kind_index(kind).map(|index| KINDS[index].0)
}

/// The line printed before the first node enters the graph.
pub const START_LINE: &str = "Scan running...";

/// One scan run's live progress: the resolved gate, the writer the stream
/// reaches, the per-kind tallies, and the per-language node tally the closing
/// summary reports.
///
/// The gate is the resolved terminal/flag decision, so every method is a no-op
/// while it is false and the caller never branches on it.
// Fields land with the struct; the methods that read them arrive in the
// following plan tasks, so the dead-code allowance is scoped to this definition.
#[allow(dead_code)]
pub struct Progress<'a> {
    /// Whether the stream is enabled (stderr is a terminal and the run carries
    /// no `--no-progress` flag).
    gate: bool,
    /// Where the start line, the per-node symbols, and the summary go: stderr
    /// in a real scan, an in-memory buffer under test.
    out: &'a mut dyn Write,
    /// Per-kind tallies, indexed by the kind's position in [`KINDS`].
    counts: [usize; KINDS.len()],
    /// The per-language node tally, one `(language, node count)` entry per
    /// language in scan order.
    languages: Vec<(String, usize)>,
    /// The [`Self::languages`] bucket later node admissions count against: the
    /// language [`Progress::language`] last selected.
    current: Option<usize>,
}

impl<'a> Progress<'a> {
    /// One run's progress stream: the resolved gate and the writer every line
    /// reaches (stderr in a real scan, an in-memory buffer under test).
    pub fn new(gate: bool, out: &'a mut dyn Write) -> Self {
        Self {
            gate,
            out,
            counts: [0; KINDS.len()],
            languages: Vec::new(),
            current: None,
        }
    }

    /// Print [`START_LINE`] before the first node enters the graph. A no-op
    /// while the gate is off.
    pub fn start(&mut self) {
        if self.gate {
            let _ = writeln!(self.out, "{START_LINE}");
        }
    }

    /// Record a scanned language: establish its bucket in the closing
    /// per-language tally and make it the language subsequent `node`
    /// admissions count against. Re-selecting an already-recorded language
    /// keeps its position and running count. A no-op while the gate is off.
    pub fn language(&mut self, language: &str) {
        if !self.gate {
            return;
        }
        let index = match self.languages.iter().position(|(name, _)| name == language) {
            Some(index) => index,
            None => {
                self.languages.push((language.to_string(), 0));
                self.languages.len() - 1
            }
        };
        self.current = Some(index);
    }
}
