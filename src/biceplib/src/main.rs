//! The `bicepfrontend` binary: a thin wrapper over the library crate. Run it with
//! `bicepfrontend <dir>`.

fn main() {
    bicepfrontend::diag::install_panic_hook();
    let _args: Vec<String> = std::env::args().skip(1).collect();
}
