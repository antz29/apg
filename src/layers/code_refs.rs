//! Code-endpoint validation (SPEC §4.1): classifying an `implemented-by`
//! code FQN against the scanned graph and the plan's planned-node set, with
//! the language-root tolerance that keeps the durable spec language-agnostic.

use std::collections::BTreeSet;

// ---------------------------------------------------------------------------
// SPEC §4.1 — code-endpoint validation (phase-3 task-10)
// ---------------------------------------------------------------------------

/// The status of one `implemented-by` code FQN against the scanned graph
/// (SPEC §4.1 "code endpoints are exempt"): a code FQN is language-native and
/// opaque — never parsed, never layer-classified. Exact string set membership
/// against the two caller-supplied universes is the whole check.
// (Unused until ingest_tree, phase-3 task-15, validates implemented-by targets.)
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeRefStatus {
    /// The FQN resolves in the scanned graph — the code exists.
    Real,
    /// The FQN is declared as a planned node in `.trans` but not yet scanned —
    /// pending, not an error (it realizes once the code lands).
    Pending,
    /// The FQN is in neither universe — the code is gone from the scanned
    /// graph (spec drift).
    Drift,
}

/// The `lang_switch` scan identities a code FQN can be rooted under — the
/// complete set `available_languages`/`id_prefix_for` recognise (`ts` and `js`
/// are the unified JS/TS frontend's two ids; a JS-only repo scans under `js`).
/// Used only to resolve an authored code reference against the scanned universe
/// tolerantly (see [`resolves_in_scanned`]).
pub const LANGUAGE_ROOTS: [&str; 9] = [
    "rust", "java", "go", "cpp", "csharp", "ts", "js", "py", "md",
];

/// True when `fqn` resolves in `scanned`, tolerating the language root: an
/// authored reference may be stored **un-rooted** (`apg.cmd_scan`) while the
/// scanned universe is rooted (`rust.apg.cmd_scan`), or the reverse. This keeps
/// the durable spec language-agnostic so a binary that predates rooting (the
/// parent) and a rooted binary (the child) resolve the *same* authored target —
/// neither depends on the other. Exact membership always wins first, then a
/// `<root>.` prefix add/strip for every known language root.
fn resolves_in_scanned(fqn: &str, scanned: &BTreeSet<String>) -> bool {
    if scanned.contains(fqn) {
        return true;
    }
    for root in LANGUAGE_ROOTS {
        if scanned.contains(&format!("{root}.{fqn}")) {
            return true;
        }
        if let Some(rest) = fqn.strip_prefix(root).and_then(|s| s.strip_prefix('.'))
            && scanned.contains(rest)
        {
            return true;
        }
    }
    false
}

/// The **language-agnostic identity** of a code FQN: strip ONE leading known
/// language root (`rust.`/`java.`/`go.`/`cpp.`/`csharp.`/`ts.`/`js.`/`py.`/`md.`)
/// and return the remainder; anything else is returned unchanged. This is the
/// stable identity the durable spec is authored against, so a rooted FQN
/// (`rust.apg.cache`) and its bare counterpart (`apg.cache`) compare EQUAL.
/// [`resolves_in_scanned`] applies the same tolerance to universe membership;
/// this exposes it to the verify paths that compare two authored code FQNs to
/// each other (planned-node realization, derived coverage) rather than to a
/// scanned universe.
pub fn code_identity(fqn: &str) -> &str {
    for root in LANGUAGE_ROOTS {
        if let Some(rest) = fqn.strip_prefix(root).and_then(|s| s.strip_prefix('.')) {
            return rest;
        }
    }
    fqn
}

/// Classify one `implemented-by` code FQN (SPEC §4.1): [`CodeRefStatus::Real`]
/// if `fqn` resolves in `scanned` (see [`resolves_in_scanned`] — rooting
/// tolerant); [`CodeRefStatus::Pending`] if it is in `planned` (and not
/// `scanned`); [`CodeRefStatus::Drift`] otherwise. `scanned` is the set of code
/// FQNs the scan produced; `planned` is the set of FQNs the plan declared as
/// planned nodes (`.trans`).
///
/// **The scanned graph is the stronger check**: membership in `scanned` wins
/// over membership in `planned` — a FQN in both universes is `Real` (it has
/// landed; the plan's planned-node declaration is moot).
// (Unused until ingest_tree, phase-3 task-15, validates implemented-by targets.)
#[allow(dead_code)]
pub fn classify_code_ref(
    fqn: &str,
    scanned: &BTreeSet<String>,
    planned: &BTreeSet<String>,
) -> CodeRefStatus {
    if resolves_in_scanned(fqn, scanned) {
        CodeRefStatus::Real
    } else if planned.contains(fqn) {
        CodeRefStatus::Pending
    } else {
        CodeRefStatus::Drift
    }
}

/// Validate a batch of `implemented-by` code FQNs against the scanned graph
/// (SPEC §4.1): returns Ok if every ref is [`CodeRefStatus::Real`] or
/// [`CodeRefStatus::Pending`]. A Pending ref is expected until the code lands
/// (the scan later realizes it), never an error. Only a
/// [`CodeRefStatus::Drift`] ref errors — the FQN is gone from the scanned graph
/// (spec drift). Bails on the first Drift, naming the offending FQN. Pure — no
/// I/O; the caller supplies both universes.
// (Unused until ingest_tree, phase-3 task-15, validates implemented-by targets.)
#[allow(dead_code)]
pub fn validate_code_refs(
    refs: &[&str],
    scanned: &BTreeSet<String>,
    planned: &BTreeSet<String>,
) -> anyhow::Result<()> {
    for fqn in refs {
        if classify_code_ref(fqn, scanned, planned) == CodeRefStatus::Drift {
            anyhow::bail!("spec drift: code FQN `{fqn}` is gone from the scanned graph");
        }
    }
    Ok(())
}
