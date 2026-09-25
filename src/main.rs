//! Thin binary entry point. The root crate's production modules and their
//! inline tests live in the library (`src/lib.rs`, module `rust.apg`); this
//! binary only dispatches into it.

fn main() {
    apg::main()
}
