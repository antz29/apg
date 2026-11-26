//! The crate's unit/int test root: pure in-memory tests in inner `unit` and
//! `int` tier submodules (each starting with `use super::*;`), with any
//! shared fixtures and helpers at this root. e2e tests live outside this
//! tree in `tests/terraformlib_e2e.rs`, under `mod e2e`.
