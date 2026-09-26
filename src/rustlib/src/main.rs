//! apg Rust scanner frontend. Parses a Cargo workspace with rust-analyzer (the
//! exact-fidelity resolver stack) and streams the unified JSONL schema (SPEC §2)
//! to stdout. Exact tier: calls and types resolve through `hir::Semantics`;
//! anything that is not a project symbol becomes an `unresolved_call` /
//! `unresolved_use` edge with a category, never a fabricated FQN.
//!
//! Method parenting: impl methods (inherent and trait) hang under their **self
//! type**; trait declarations and default methods hang under the **trait**.
//! `resolve_method_call` resolves a call on a concrete receiver to the impl
//! block's function, so internal calls land on the exact declared node, and
//! same-trait/different-type impls render distinct FQNs instead of colliding on
//! the trait.

mod decl;
mod discovery;
mod emit;
mod identity;
mod load;
mod record;
mod scanner;
mod source;
mod state;
mod walk;

// ── CLI ───────────────────────────────────────────────────────────────

fn main() {
    if let Err(e) = scanner::run(std::env::args().collect()) {
        eprintln!("rustfrontend: {e:#}");
        std::process::exit(1);
    }
}
