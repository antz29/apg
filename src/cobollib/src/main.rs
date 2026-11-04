//! The `cobolfrontend` binary: a thin wrapper over the library crate, which owns the
//! COBOL scanner. Run it with `cobolfrontend <dir> [--module <m>] [--id-prefix <p>] ...`.
//! A failed run ends stderr with one `cobolfrontend: <reason>` line and exits non-zero.

fn main() {
    // Replace the default multi-line panic report so the reason is the last stderr line.
    std::panic::set_hook(Box::new(|info| {
        let payload = info.payload();
        let reason = payload
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "panic".to_string());
        eprintln!("cobolfrontend: {reason}");
    }));
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(message) = cobolfrontend::scan::run(&args) {
        eprintln!("cobolfrontend: {message}");
        std::process::exit(1);
    }
}
