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
            TimingReport::from_machine_line(r#"[timing-json] {"type":"scan_timing"}"#).is_none(),
            "missing phase durations must not parse"
        );
    }
}
