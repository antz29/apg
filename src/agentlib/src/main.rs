//! The `agentfrontend` binary: a thin wrapper over the library crate, which
//! owns the agent scanner's module tree and crate-internal API. Run it with
//! `agentfrontend <dir> [--id-prefix <p>] [--exclude-path <glob>]...
//! [exclude...]`.

fn main() {
    agentfrontend::main();
}
