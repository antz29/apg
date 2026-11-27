//! The `terraformfrontend` binary: a thin wrapper over the library crate, which owns the
//! Terraform scanner. Run it with `terraformfrontend <dir> [--module <m>]... [--id-prefix <p>]
//! [--targets <file>] [--cache-dir <d>] [--cache-key <k>] [exclude...]`.
//! A failed run ends stderr with one `terraformfrontend: <reason>` line and exits non-zero.

fn main() {
    // Replace the default multi-line panic report so the reason is the last stderr line.
    std::panic::set_hook(Box::new(|info| {
        let payload = info.payload();
        let reason = payload
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "panic".to_string());
        eprintln!("terraformfrontend: {reason}");
    }));
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(message) = terraformfrontend::scan::run(&args) {
        eprintln!("terraformfrontend: {message}");
        std::process::exit(1);
    }
}
