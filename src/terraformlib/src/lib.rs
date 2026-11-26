//! terraformfrontend: the Terraform frontend for apg. It parses Terraform
//! sources (HCL native and JSON syntax) with hcl-edit and emits unified JSONL
//! facts on stdout. It is self-contained and spawns no child processes.

pub mod source;
