//! The COBOL scan driver: the effective dialect for a scan (`--std`, else
//! `cobol.dialect` in `apg/config.json`, else `cobol85`) and the argv fragment
//! the `cobolfrontend` spawn receives. Pure: the caller reads the config text.

use anyhow::{Context, Result, bail};

/// The dialects a scan may run under. `cobol2023` is a refused placeholder.
const DIALECTS: [&str; 4] = ["cobol85", "cobol2002", "cobol2014", "ibm"];

/// The dialect used when neither `--std` nor `cobol.dialect` names one.
pub const DEFAULT_DIALECT: &str = "cobol85";

/// Validates a dialect name. `cobol2023` and unknown names are refused with the
/// same messages the `cobolfrontend` dialect table carries.
pub fn validate_dialect(name: &str) -> Result<&'static str> {
    if let Some(d) = DIALECTS.iter().find(|d| **d == name) {
        return Ok(d);
    }
    if name == "cobol2023" {
        bail!(
            "dialect cobol2023 is a placeholder with no features yet; use cobol85, cobol2002, cobol2014 or ibm"
        );
    }
    bail!("unknown dialect `{name}`; expected cobol85, cobol2002, cobol2014 or ibm")
}

/// The scan's effective dialect. `std_flag` (the `--std` value) wins for the
/// run; otherwise `config_text` (the contents of `apg/config.json`, when it
/// exists) supplies `cobol.dialect`; otherwise [`DEFAULT_DIALECT`].
pub fn effective_dialect(
    std_flag: Option<&str>,
    config_text: Option<&str>,
) -> Result<&'static str> {
    if let Some(name) = std_flag {
        return validate_dialect(name);
    }
    if let Some(text) = config_text {
        let config: serde_json::Value =
            serde_json::from_str(text).context("apg/config.json is not valid JSON")?;
        if let Some(value) = config.pointer("/cobol/dialect") {
            let name = value
                .as_str()
                .context("cobol.dialect in apg/config.json must be a string")?;
            return validate_dialect(name);
        }
    }
    Ok(DEFAULT_DIALECT)
}

/// The argv fragment for the cobol spawn: `-std=<dialect>` then one
/// `--copy-path <dir>` pair per copy directory.
pub fn frontend_args(dialect: &str, copy_paths: &[String]) -> Vec<String> {
    let mut args = vec![format!("-std={dialect}")];
    for dir in copy_paths {
        args.push("--copy-path".to_string());
        args.push(dir.clone());
    }
    args
}
