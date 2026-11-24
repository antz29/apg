use std::path::Path;

use super::*;

/// unit tier — pure in-memory: exactly [`rebuild_catching_panic`] under test,
/// the rebuild faked by an in-memory closure; no filesystem, database, git or
/// process I/O.
mod unit {
    use super::*;

    #[test]
    fn rebuild_panic_is_reported_as_an_error() {
        let dir = Path::new("/nonexistent/main");

        // A panicking rebuild becomes an `Err` naming the panic, carrying the
        // payload — so it takes the same rollback path as a returned error.
        let panicking = |_: &Path| -> anyhow::Result<()> { panic!("boom") };
        let err = rebuild_catching_panic(dir, &panicking).expect_err("a panic must become Err");
        let msg = err.to_string();
        assert!(msg.contains("main rebuild panicked"), "message was: {msg}");
        assert!(msg.contains("boom"), "message was: {msg}");

        // A successful rebuild passes through.
        let ok = |_: &Path| -> anyhow::Result<()> { Ok(()) };
        assert!(rebuild_catching_panic(dir, &ok).is_ok());

        // A returned error passes through unchanged.
        let failing = |_: &Path| -> anyhow::Result<()> { Err(anyhow::anyhow!("returned failure")) };
        let err =
            rebuild_catching_panic(dir, &failing).expect_err("a returned Err must pass through");
        assert!(
            err.to_string().contains("returned failure"),
            "message was: {err}"
        );
    }
}
