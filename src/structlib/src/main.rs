//! The `structfrontend` binary: a thin wrapper over the library crate, which
//! owns the structural scanner's module tree and crate-internal API. Run it
//! with `structfrontend <dir> [--stream <id>] [--module <dir>]... [--id-prefix
//! <p>] [--targets <file>] [--cache-dir <dir>] [--cache-key <key>] [exclude...]`.

fn main() {
    structfrontend::main();
}
