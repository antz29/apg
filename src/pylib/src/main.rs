// The `pyfrontend` process entry point.
//
// The Python scanner lives in the `pyfrontend` library (`src/lib.rs`); this
// binary is only the process wrapper. It parses the command line through the
// library's `parse_args` and dispatches to the library's `run`, exiting
// non-zero with the same diagnostics the scanner has always emitted.

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let args = match pyfrontend::parse_args(&argv) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    if let Err(e) = pyfrontend::run(args) {
        eprintln!("pyfrontend: {e}");
        std::process::exit(1);
    }
}
