//! Live `apg scan` progress on stderr: the start line, one kind symbol per
//! node as it enters the graph, and the closing per-language/per-kind summary
//! carrying the symbol legend inline.
//!
//! Progress reaches stderr alone (never `apg-frontend.log`), and the whole
//! stream is gated on stderr being a terminal and the run carrying no
//! `--no-progress` flag.

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
