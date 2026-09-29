//! Deterministic spec lint (`apg spec lint`) and the shared advisory wording
//! classifier.
//!
//! `apg spec lint` is the read-only, whole-durable-spec integrity check that
//! sits beside review. It reports the mechanical R2/R3/R4 violations and the
//! spec-delta gates as **errors**, and emits non-blocking advisory wording
//! warnings for tier-1-3 node bodies a reviewer is likely to reject.
//!
//! The delta arm reuses phase 1's merge-base durable-spec delta
//! (`crate::plan_cmd::change_set_spec_delta` / `SpecDelta`) and its
//! requirement-Satisfies gate (`crate::plan_cmd::coverage_check`) — one source
//! of truth, never a reimplementation. `wording_warning` is the single
//! definition of the tier-1-3 wording advisory: the lint and the
//! `apg node add` / `apg node update` write hooks all consume it. The linter is
//! read-only — it never writes.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::layers::NodeFile;

/// The advisory message [`wording_warning`] returns for likely-flagged
/// tier-1-3 wording — the single source of truth both the lint and the
/// `apg node add` / `apg node update` write hooks print.
pub const WORDING_ADVISORY: &str =
    "this wording appears likely to be flagged in review — would you like to re-phrase?";

/// The shared advisory wording classifier over a tier-1-3 node body (R1/R5):
/// returns [`WORDING_ADVISORY`] when `body` carries negation, future-tense or
/// obligation, or time-relative wording a reviewer is likely to reject, and
/// `None` for plain present-tense prose. Pure — no I/O.
///
/// Words are matched as whole tokens (case-insensitively), so `note` and
/// `non-note` never trip the negation token `no`, while `no longer` does (its
/// `no` token). Tier-1-3 constraints are excluded by the lint, not here: a
/// negative rule legitimately lives in a layer-scoped constraint, so this
/// classifier stays a pure predicate over a body.
pub fn wording_warning(body: &str) -> Option<&'static str> {
    const NEGATION: [&str; 4] = ["not", "never", "no", "without"];
    const FUTURE: [&str; 5] = ["will", "shall", "must", "should", "would"];
    const TIME_RELATIVE: [&str; 5] = ["today", "now", "currently", "was", "previously"];
    let lowered = body.to_lowercase();
    let flagged = lowered.split(|c: char| !c.is_alphanumeric()).any(|tok| {
        NEGATION.contains(&tok) || FUTURE.contains(&tok) || TIME_RELATIVE.contains(&tok)
    });
    if flagged {
        Some(WORDING_ADVISORY)
    } else {
        None
    }
}

/// The shared tier-1-3 wording-advisory selection: a node in a tier-1-3 layer
/// (`requirements`/`domain`/`solution`) that is not a constraint — a negative
/// rule legitimately lives in a layer-scoped constraint (R2) — and whose body
/// carries likely-flagged wording. [`lint`] and the `apg node add` /
/// `apg node update` write hooks both consume this, so their advisory sets
/// agree by construction. `pub(crate)` — the write surface's single source of
/// truth for the selection, never a public unit.
pub(crate) fn tier_body_warning(
    layer_dir: &str,
    node_type: &str,
    body: &str,
) -> Option<&'static str> {
    let is_tier = matches!(layer_dir, "requirements" | "domain" | "solution");
    if is_tier && node_type != "constraint" {
        wording_warning(body)
    } else {
        None
    }
}

/// The whole-durable-spec deterministic check. Returns `(errors, advisories)`:
/// the blocking rule/delta-gate violations first, the non-blocking tier-1-3
/// wording advisories second. Read-only — it reads the durable node files, the
/// merge-base git tree, and the transient plan store, and never writes.
///
/// Errors (blocking — `cmd_spec` reports them and exits non-zero):
/// - **R2** — a constraint's layer is its scope and it names no node, so a
///   constraint carrying `attaches-to` is a violation
///   (`global.constraint.spec-constraint-scope`);
/// - **R3** — a node holds at most one note (two notes detailing one target);
/// - **R4** — each note's `details` edge names exactly one non-note node;
/// - **the delta gates** — every `implemented-by` claim the change-set delta
///   adds, re-points, or removes is touched by a plan task, and every
///   requirement the delta adds or changes is `Satisfies`'d by a plan phase.
///   Both arms reuse phase 1's `plan_cmd::change_set_spec_delta` /
///   `plan_cmd::coverage_check` — never a reimplementation.
///
/// Advisories (non-blocking, R1/R5): [`wording_warning`] over every tier-1-3
/// node body except a constraint (a negative rule legitimately lives in a
/// layer-scoped constraint), plus the delta's claim-less solution nodes. The
/// plan records are read from every `apg/.trans/plans/*.jsonl` file, so the
/// requirement-Satisfies gate sees this branch's phase `Satisfies` set.
pub fn lint(apg_root: &Path) -> anyhow::Result<(Vec<String>, Vec<String>)> {
    let nodes = crate::layers::read_existing_nodes(apg_root)?;
    let mut errors: Vec<String> = Vec::new();
    let mut advisories: Vec<String> = Vec::new();

    let fqn_of =
        |n: &NodeFile| crate::layers::fqn(crate::layers::layer_of(&n.layer), &n.node_type, &n.name);
    let note_fqns: BTreeSet<String> = nodes
        .iter()
        .filter(|n| n.node_type == "note")
        .map(&fqn_of)
        .collect();

    // R2 — every constraint's scope is its layer; a constraint names no node,
    // so the off-model `attaches-to` property is a violation.
    for n in &nodes {
        if n.node_type == "constraint" && n.properties.contains_key(crate::layers::PROP_ATTACHES_TO)
        {
            errors.push(format!(
                "R2: constraint `{}` declares `{}` — a constraint's layer is its scope and it names no node",
                fqn_of(n),
                crate::layers::PROP_ATTACHES_TO
            ));
        }
    }

    // R3 — a node holds at most one note: count the notes detailing each target
    // (a note's `details` out-edge) and report every target with two or more.
    let mut notes_per_target: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for n in &nodes {
        if n.node_type != "note" {
            continue;
        }
        let note = fqn_of(n);
        for oe in &n.out {
            if oe.kind == "details" {
                notes_per_target
                    .entry(oe.target.clone())
                    .or_default()
                    .push(note.clone());
            }
        }
    }
    for (target, notes) in &notes_per_target {
        if notes.len() > 1 {
            errors.push(format!(
                "R3: node `{target}` holds {} notes ({}) — a node holds at most one note",
                notes.len(),
                notes.join(", ")
            ));
        }
    }

    // R4 — each note's `details` edge names exactly one non-note node.
    for n in &nodes {
        if n.node_type != "note" {
            continue;
        }
        let note = fqn_of(n);
        let details: Vec<&str> = n
            .out
            .iter()
            .filter(|e| e.kind == "details")
            .map(|e| e.target.as_str())
            .collect();
        if details.len() != 1 {
            errors.push(format!(
                "R4: note `{note}` has {} `details` edge(s) — a note's details edge names exactly one node",
                details.len()
            ));
            continue;
        }
        let target = details[0];
        let names_a_note = note_fqns.contains(target)
            || crate::layers::parse_fqn(target)
                .map(|(_, node_type, _)| node_type == "note")
                .unwrap_or(false);
        if names_a_note {
            errors.push(format!(
                "R4: note `{note}` details another note (`{target}`) — a note's details edge names exactly one non-note node"
            ));
        }
    }

    // Advisories — tier-1-3 bodies a reviewer is likely to reject (R1/R5). The
    // shared [`tier_body_warning`] selection exempts a constraint: a negative
    // rule lives legitimately in a layer-scoped constraint.
    for n in &nodes {
        if let Some(msg) = tier_body_warning(&n.layer, &n.node_type, &n.body) {
            advisories.push(format!("{}: {msg}", fqn_of(n)));
        }
    }

    // Delta gates — reuse phase 1's merge-base delta and requirement-Satisfies
    // check; the plan records supply the phase `Satisfies` set.
    let delta = crate::plan_cmd::change_set_spec_delta(apg_root)?;
    let mut plan_records: Vec<crate::schema::Record> = Vec::new();
    for path in crate::specs::plan_files(apg_root) {
        plan_records.extend(crate::specs::read_jsonl(&path)?);
    }
    let coverage = crate::plan_cmd::coverage_check(&plan_records, &delta);
    for gap in &coverage.gaps {
        errors.push(format!(
            "delta gate: solution node `{}` implemented-by `{}` is touched by no plan task",
            gap.solution, gap.fqn
        ));
    }
    for req in &coverage.requirement_gaps {
        errors.push(format!(
            "delta gate: requirement `{req}` (added or changed by this change-set) is `Satisfies`'d by no plan phase"
        ));
    }
    for fqn in &coverage.no_claims {
        advisories.push(format!(
            "solution node `{fqn}` has no implemented-by edge (exempt — no code claims it)"
        ));
    }

    Ok((errors, advisories))
}

/// `apg spec lint` — the argv handler for the `spec` subcommand. The only valid
/// subcommand is `lint`: resolve the layout root, run [`lint`], print the
/// non-blocking advisories and then the blocking errors to stderr, and exit
/// non-zero (via the returned error) when any error is found; a clean spec
/// prints a one-line summary to stdout. Any other subcommand is a usage error.
/// `pub` so the CLI dispatch in `main` wires it (`rust.apg.main`).
pub fn cmd_spec(args: &[String]) -> anyhow::Result<()> {
    match args.first().map(String::as_str) {
        Some("lint") => {
            let apg_root = crate::plan_cmd::require_apg_root()?;
            let (errors, advisories) = lint(&apg_root)?;
            for advisory in &advisories {
                eprintln!("apg: warning: {advisory}");
            }
            if errors.is_empty() {
                println!(
                    "spec lint: no violations ({} advisory warning(s))",
                    advisories.len()
                );
                return Ok(());
            }
            for error in &errors {
                eprintln!("apg: spec lint: {error}");
            }
            anyhow::bail!("spec lint found {} violation(s)", errors.len());
        }
        other => anyhow::bail!("usage: apg spec lint (got `{}`)", other.unwrap_or("<none>")),
    }
}
