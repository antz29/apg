//! Per-phase scan timing (`domain.entity.phase-timing-report`,
//! `requirements.requirement.per-phase-timing-report`).
//!
//! `apg scan` measures four phases — **startup/overhead**, **frontend**,
//! **ingest-assembly**, **db-load** — and reports them first-class: a human
//! `[timing]` log line plus a machine-readable `[timing-json]` line, so the
//! incremental wins (win A/B/C) can be measured against the recorded full-scan
//! baseline. On the freshness fast-path the language frontends never run, so the
//! frontend phase is reported with the `frontend-skipped` marker while all four
//! phase keys stay present.
//!
//! The four phase keys are always in the report (structurally, an array indexed
//! by [`Phase`]), so "a scan prints each phase's duration" holds on every path
//! and a reader never has to infer a missing phase.

use std::time::Duration;

/// The four scan phases, in canonical report order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Phase {
    /// Argument parsing, git state, version gate, layout, incremental
    /// preparation — everything before the first frontend spawn (or before the
    /// fast-path verdict).
    Startup,
    /// The per-language frontend spawn loop.
    Frontend,
    /// Ingest + graph assembly: the ingestor passes and the `apg/layers` tree
    /// ingestion.
    IngestAssembly,
    /// The DB build: parquet load files, `Database::new`, `create_schema`,
    /// `copy_from`, the splice, and the `graph.jsonl` export.
    DbLoad,
}

impl Phase {
    /// Every phase, in report order.
    pub(crate) const ALL: [Phase; 4] = [
        Phase::Startup,
        Phase::Frontend,
        Phase::IngestAssembly,
        Phase::DbLoad,
    ];

    /// The stable report key used by both the human and machine lines.
    pub(crate) fn key(self) -> &'static str {
        match self {
            Phase::Startup => "startup",
            Phase::Frontend => "frontend",
            Phase::IngestAssembly => "ingest-assembly",
            Phase::DbLoad => "db-load",
        }
    }

    fn index(self) -> usize {
        match self {
            Phase::Startup => 0,
            Phase::Frontend => 1,
            Phase::IngestAssembly => 2,
            Phase::DbLoad => 3,
        }
    }
}

/// The human report line's prefix.
pub(crate) const HUMAN_PREFIX: &str = "[timing]";

/// The machine-readable report line's prefix (the JSON object follows it).
pub(crate) const MACHINE_PREFIX: &str = "[timing-json] ";

/// The `frontend-skipped` marker appended to the human line when the frontend
/// phase never ran (the whole-tree freshness fast-path).
pub(crate) const FRONTEND_SKIPPED_MARKER: &str = "frontend-skipped";

/// A per-phase timing report for one scan. Every phase has a duration (zero
/// when it did no work) and the frontend phase additionally carries a
/// `frontend-skipped` flag so a fast path is distinguishable from a genuinely
/// instant frontend run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct TimingReport {
    durations: [Duration; 4],
    frontend_skipped: bool,
}

impl TimingReport {
    pub(crate) fn new() -> TimingReport {
        TimingReport::default()
    }

    /// Records one phase's measured duration, replacing any previous value.
    pub(crate) fn record(&mut self, phase: Phase, elapsed: Duration) {
        self.durations[phase.index()] = elapsed;
    }

    /// Accumulates more time into a phase (the per-language frontends add up).
    pub(crate) fn add(&mut self, phase: Phase, elapsed: Duration) {
        self.durations[phase.index()] += elapsed;
    }

    /// The recorded duration of `phase`.
    pub(crate) fn duration(&self, phase: Phase) -> Duration {
        self.durations[phase.index()]
    }

    /// Marks the frontend phase `frontend-skipped` (the whole-tree freshness
    /// fast-path ran no frontend).
    pub(crate) fn mark_frontend_skipped(&mut self) {
        self.frontend_skipped = true;
    }

    /// Whether the frontend phase was skipped this scan.
    pub(crate) fn frontend_skipped(&self) -> bool {
        self.frontend_skipped
    }

    /// The human log line: `[timing] startup=<s>s frontend=<s>s
    /// ingest-assembly=<s>s db-load=<s>s`, with the `frontend-skipped` marker
    /// appended when the frontends never ran. All four phase keys are always
    /// present.
    pub(crate) fn human_line(&self) -> String {
        let mut line = String::from(HUMAN_PREFIX);
        for phase in Phase::ALL {
            line.push(' ');
            line.push_str(phase.key());
            line.push('=');
            line.push_str(&format!("{:.3}s", self.duration(phase).as_secs_f64()));
        }
        if self.frontend_skipped() {
            line.push(' ');
            line.push_str(FRONTEND_SKIPPED_MARKER);
        }
        line
    }

    /// The machine-readable JSON line (prefixed by [`MACHINE_PREFIX`]). Durations
    /// are nanoseconds so the line round-trips exactly through
    /// [`TimingReport::from_machine_line`].
    pub(crate) fn machine_line(&self) -> String {
        let json = serde_json::json!({
            "type": "scan_timing",
            "startup_ns": self.duration(Phase::Startup).as_nanos() as u64,
            "frontend_ns": self.duration(Phase::Frontend).as_nanos() as u64,
            "ingest_assembly_ns": self.duration(Phase::IngestAssembly).as_nanos() as u64,
            "db_load_ns": self.duration(Phase::DbLoad).as_nanos() as u64,
            "frontend_skipped": self.frontend_skipped(),
        });
        format!("{MACHINE_PREFIX}{json}")
    }

    /// Parses a [`TimingReport::machine_line`] back into a report. Returns
    /// `None` for anything that is not a complete `scan_timing` object (so a
    /// malformed or foreign line is never mistaken for a report). The parser is
    /// the report model's reader and is exercised by the in-process round-trip
    /// tests (task-6) and the real-scan acceptance test (task-11).
    #[cfg(test)]
    pub(crate) fn from_machine_line(line: &str) -> Option<TimingReport> {
        let line = line.trim();
        let json = line.strip_prefix(MACHINE_PREFIX).unwrap_or(line);
        let value: serde_json::Value = serde_json::from_str(json).ok()?;
        if value.get("type")?.as_str()? != "scan_timing" {
            return None;
        }
        let mut report = TimingReport::new();
        for (phase, key) in [
            (Phase::Startup, "startup_ns"),
            (Phase::Frontend, "frontend_ns"),
            (Phase::IngestAssembly, "ingest_assembly_ns"),
            (Phase::DbLoad, "db_load_ns"),
        ] {
            report.record(phase, Duration::from_nanos(value.get(key)?.as_u64()?));
        }
        if value.get("frontend_skipped")?.as_bool()? {
            report.mark_frontend_skipped();
        }
        Some(report)
    }
}

/// The two phase durations [`crate::run_pipeline`] owns and hands back to
/// `cmd_scan` for the report (phase-04 task-4).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct PipelineTimings {
    /// Ingest + graph assembly inside the pipeline.
    pub ingest_assembly: Duration,
    /// The DB build (splice OR full load) + `graph.jsonl`.
    pub db_load: Duration,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// unit tier -- pure in-memory: the report model only, no scan.
    mod unit {
        use super::*;

        /// Phase-04 task-6: all four phase durations are present on every report
        /// and the machine-readable line round-trips exactly.
        #[test]
        fn report_carries_all_four_phases_and_machine_line_round_trips() {
            let mut report = TimingReport::new();
            report.record(Phase::Startup, Duration::from_micros(1_234));
            report.record(Phase::Frontend, Duration::from_millis(9_001));
            report.record(Phase::IngestAssembly, Duration::from_micros(7));
            report.record(Phase::DbLoad, Duration::from_nanos(250));

            // Every phase key appears in the human line.
            let human = report.human_line();
            assert!(human.starts_with(HUMAN_PREFIX));
            for phase in Phase::ALL {
                assert!(
                    human.contains(&format!("{}=", phase.key())),
                    "the human line must carry {}: {human}",
                    phase.key()
                );
            }

            // The machine line carries all four durations and round-trips.
            let machine = report.machine_line();
            assert!(machine.starts_with(MACHINE_PREFIX));
            let back = TimingReport::from_machine_line(&machine).expect("the line must parse");
            assert_eq!(back, report);
            for phase in Phase::ALL {
                assert_eq!(back.duration(phase), report.duration(phase));
            }
        }

        /// Phase-04 task-6: the fast-path `frontend-skipped` marker is set on a
        /// skip and clear on a normal scan (the flag drives both lines).
        #[test]
        fn frontend_skip_marker_sets_and_clears() {
            let mut report = TimingReport::new();
            assert!(!report.frontend_skipped());
            assert!(!report.human_line().contains(FRONTEND_SKIPPED_MARKER));

            report.mark_frontend_skipped();
            assert!(report.frontend_skipped());
            assert!(report.human_line().contains(FRONTEND_SKIPPED_MARKER));
            let back = TimingReport::from_machine_line(&report.machine_line()).unwrap();
            assert!(back.frontend_skipped(), "the flag must round-trip");
            assert_eq!(back, report);

            // A normal scan (a fresh report) has no marker.
            assert!(!TimingReport::new().frontend_skipped());
        }

        /// `add` accumulates a phase (the per-language frontends add up) and the
        /// parser rejects anything that is not a complete timing line.
        #[test]
        fn add_accumulates_and_incomplete_lines_are_rejected() {
            let mut report = TimingReport::new();
            report.add(Phase::Frontend, Duration::from_millis(10));
            report.add(Phase::Frontend, Duration::from_millis(5));
            assert_eq!(report.duration(Phase::Frontend), Duration::from_millis(15));

            assert!(TimingReport::from_machine_line("[timing] startup=0.000s").is_none());
            assert!(
                TimingReport::from_machine_line(r#"[timing-json] {"type":"scan_timing"}"#)
                    .is_none(),
                "missing phase durations must not parse"
            );
        }
    }
}
