//! The `cobolfrontend` binary: a thin wrapper over the library crate, which owns the
//! COBOL scanner. Run it with `cobolfrontend <dir> [--module <m>] [--id-prefix <p>] ...`.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(message) = cobolfrontend::scan::run(&args) {
        eprintln!("{message}");
        std::process::exit(1);
    }
}
