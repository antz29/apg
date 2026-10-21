//! Live `apg scan` progress on stderr: the start line, one kind symbol per
//! node as it enters the graph, and the closing per-language/per-kind summary
//! carrying the symbol legend inline.
//!
//! Progress reaches stderr alone (never `apg-frontend.log`), and the whole
//! stream is gated on stderr being a terminal and the run carrying no
//! `--no-progress` flag.

use std::io::Write;

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
        }
    }
}
