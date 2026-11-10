//! csharp-projects (e2e): the C# frontend's project-aware and Razor scan
//! tier, driving the staged `csharpfrontend` / the CANDIDATE binary
//! (`testutil::apg_bin`) against scratch `/tmp` trees only — never the apg
//! repo. Every test is `#[ignore]`d and runs through `cargo test-e2e`.
