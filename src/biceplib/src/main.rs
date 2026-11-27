//! The `bicepfrontend` binary: a thin wrapper over the library crate, which owns the Bicep
//! scanner. Run it with `bicepfrontend <dir> [--module <m>]... [--id-prefix <p>]
//! [--targets <file>] [--cache-dir <d>] [--cache-key <k>] [exclude...]`. A failed run ends
//! stderr with one `bicepfrontend: <reason>` line and exits non-zero.

fn main() {
    bicepfrontend::diag::install_panic_hook();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(message) = bicepfrontend::scan::run(&args) {
        eprintln!("bicepfrontend: {message}");
        std::process::exit(1);
    }
}
