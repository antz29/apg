use super::*;

/// unit tier -- pure in-memory: no filesystem, database, git or process.
mod unit {
    use super::*;

    #[test]
    fn iso8601_formats_known_epochs() {
        assert_eq!(iso8601(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso8601(1_700_000_000), "2023-11-14T22:13:20Z");
        // The live formatter produces the same shape.
        let now = now_iso8601();
        assert_eq!(now.len(), "YYYY-MM-DDTHH:MM:SSZ".len());
        assert!(now.ends_with('Z'));
        assert!(now.as_bytes()[10] == b'T');
    }
}
