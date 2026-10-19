//! Deterministic spec lint (`apg spec lint`) and the shared advisory wording
//! classifier.
//!
//! `apg spec lint` is the read-only, whole-durable-spec integrity check that
//! sits beside review. It reports the shared mechanical validator set (the
//! SPEC §4 edge-kind matrix, the event-only `publishes`/`subscribes` target
//! rule, and the whole-model cardinalities — the SAME functions the write path
//! calls, never a reimplementation), the mechanical R2/R3/R4 violations, and
//! the tier-1-3 body-budget rule as **errors**, and emits non-blocking advisory
//! wording warnings for tier-1-3 node bodies a reviewer is likely to reject.
//!
//! **Scope** (`--tier`) and **context** are orthogonal (SPEC §9.2): a single
//! tier scopes which rules run (its local structure plus the typed upstream
//! spine paths it requires), while the context is always ordinary lint (a
//! missing Component `implemented-by` is permitted, SPEC §8.2). Save and
//! merge-strength contexts are internal caller choices of the shared
//! validator's `Context`, never lint flags.
//!
//! Ordinary lint excludes the whole-model change-set delta coverage gates
//! (SPEC §9.3): they are plan-verification concerns, owned by
//! `crate::plan_cmd::change_set_spec_delta` / `coverage_check` behind
//! `apg plan verify`, never additional lint structural rules.
//! `wording_warning` is the single definition of the tier-1-3 wording advisory:
//! the lint and the `apg node add` / `apg node update` write hooks all consume
//! it. The linter is read-only — it never writes.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::layers::{
    Context, Layer, NodeFile, fqn, layer_of, parse_fqn, validate_cardinalities, validate_edges,
    validate_event_targets,
};

/// The `--tier` scope of one lint run (SPEC §9.4): absent means all tiers.
const TIER_FLAG: &str = "--tier";

/// Resolve a `--tier` value to its [`Layer`] (SPEC §9.4): `requirements`,
/// `domain`, or `solution`. `implementation` is outside the tier-1-3
/// specification this lint surface implements, so it is a usage error rather
/// than a silent no-op; every other value is invalid.
fn parse_tier(value: &str) -> anyhow::Result<Layer> {
    match value {
        "requirements" => Ok(Layer::Requirements),
        "domain" => Ok(Layer::Domain),
        "solution" => Ok(Layer::Solution),
        "implementation" => anyhow::bail!(
            "usage: `--tier implementation` is outside the tier-1-3 spec-lint surface — expected requirements|domain|solution"
        ),
        other => anyhow::bail!(
            "usage: invalid `--tier` value `{other}` — expected requirements|domain|solution"
        ),
    }
}

/// Parse `apg spec lint`'s flags (SPEC §9.4): the optional `--tier <tier>`
/// scope, or all tiers when omitted. Unknown flags, a repeated `--tier`, a
/// missing/invalid tier value, and unexpected positional arguments are usage
/// errors.
fn parse_lint_args(args: &[String]) -> anyhow::Result<Option<Layer>> {
    let mut tier: Option<Layer> = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            TIER_FLAG => {
                if tier.is_some() {
                    anyhow::bail!("usage: `--tier` may be given at most once");
                }
                let Some(value) = iter.next() else {
                    anyhow::bail!(
                        "usage: `--tier` requires a value (requirements|domain|solution)"
                    );
                };
                tier = Some(parse_tier(value)?);
            }
            other if other.starts_with('-') => {
                anyhow::bail!("usage: unknown flag `{other}` for `apg spec lint`")
            }
            other => anyhow::bail!(
                "usage: unexpected argument `{other}` for `apg spec lint` — the surface is `apg spec lint [--tier requirements|domain|solution]`"
            ),
        }
    }
    Ok(tier)
}

/// The assembled FQN → node-file view the shared validators take.
fn assembled_nodes(nodes: &[NodeFile]) -> BTreeMap<String, &NodeFile> {
    nodes
        .iter()
        .map(|n| (fqn(layer_of(&n.layer), &n.node_type, &n.name), n))
        .collect()
}

/// The shared mechanical validator set over the durable model (SPEC §9.1):
/// the SPEC §4 edge-kind matrix ([`validate_edges`]), the event-only
/// `publishes`/`subscribes` target rule ([`validate_event_targets`]) plus the
/// whole-model cardinalities ([`validate_cardinalities`]) — the SAME functions
/// the write path calls, never a reimplementation. Returns one message per
/// violation.
///
/// `tier` is the **scope** ([`Layer`] or all tiers) and `context` is the
/// **consistency boundary** (SPEC §9.2): scope selects which tier's rules are
/// checked, context only governs the deferred Component `implemented-by`
/// presence rule (SPEC §8.2).
///
/// A scoped run validates the edges whose source is in scope (the tier's local
/// structure) plus the typed cross-tier edges whose target is in scope and
/// whose source is an upstream tier (the upstream spine paths the tier
/// requires — e.g. `driven-by` from a Domain Service into a Requirements
/// Requirement). Every
/// validated edge's authored endpoints must exist in the model — endpoint
/// existence is structural (drift), not an out-of-scope "downstream
/// completeness" question, so a single-tier run flags a dangling authored
/// endpoint on the edges it validates (SPEC §9.2).
/// Pure — no I/O.
pub(crate) fn model_errors(
    nodes: &[NodeFile],
    tier: Option<Layer>,
    context: Context,
) -> Vec<String> {
    let assembled = assembled_nodes(nodes);
    let mut errors: Vec<String> = Vec::new();

    // Tier rank for the upstream-spine filter: requirements < domain <
    // solution. Under P3, cross-tier edges point "up" (concrete → abstract),
    // so an in-scope tier's upstream-spine edges have their SOURCE in a more
    // concrete tier (a higher rank) and their TARGET in the in-scope tier —
    // e.g. `driven-by` from a Domain Service into a Requirements Requirement.
    let rank = |layer: Layer| match layer {
        Layer::Requirements => Some(1u8),
        Layer::Domain => Some(2u8),
        Layer::Solution => Some(3u8),
        _ => None,
    };

    let mut edges: Vec<(&str, &str, &str)> = Vec::new();
    for (from, node) in &assembled {
        let src_layer = layer_of(&node.layer);
        for edge in &node.out {
            let in_scope = tier.is_none_or(|scope| src_layer == scope);
            let upstream = tier.is_some_and(|scope| {
                parse_fqn(&edge.target).is_ok_and(|(dst_layer, _, _)| {
                    dst_layer == scope
                        && rank(src_layer)
                            .zip(rank(dst_layer))
                            .is_some_and(|(s, d)| s > d)
                })
            });
            if in_scope || upstream {
                edges.push((edge.kind.as_str(), from.as_str(), edge.target.as_str()));
            }
        }
    }

    // Matrix + endpoint identity/direction. One `validate_edges` call per edge
    // so lint reports every violation (the write path's batch stops at the
    // first) while still running the one shared implementation.
    let mut missing: BTreeSet<String> = BTreeSet::new();
    for triple in &edges {
        if let Err(err) = validate_edges(std::slice::from_ref(triple)) {
            errors.push(err.to_string());
        }
        // The property half of the `publishes`/`subscribes` matrix rows: an
        // event-only target is not expressible in `validate_edges` (it never
        // sees properties), so run the write path's shared
        // `validate_event_targets` over the same selected edges — the lint
        // reports the identical message the write surface does.
        if let Err(err) = validate_event_targets(std::slice::from_ref(triple), &assembled) {
            errors.push(err.to_string());
        }
        let (kind, source, target) = *triple;
        for endpoint in [source, target] {
            if parse_fqn(endpoint).is_ok() && !assembled.contains_key(endpoint) {
                missing.insert(format!(
                    "edge `{kind}` endpoint `{endpoint}` is not a node of the model (dangling authored reference)"
                ));
            }
        }
    }
    errors.extend(missing);

    errors.extend(validate_cardinalities(&assembled, tier, context));
    errors
}

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

/// The note-confinement rule (`global.constraint.note-implementation-only`):
/// a `note` attaches only to implementation-layer code, so every `note` node
/// whose layer is not `implementation` yields one error string naming the
/// rule. Pure — no I/O — so the unit tier pins it over in-memory [`NodeFile`]s;
/// [`lint`] feeds it the nodes it read from the durable layer files.
/// `pub(crate)` — the lint's single source of truth for the rule, never a
/// public unit.
pub(crate) fn note_confinement_errors(nodes: &[NodeFile]) -> Vec<String> {
    nodes
        .iter()
        .filter(|n| n.node_type == "note" && n.layer != "implementation")
        .map(|n| {
            let fqn =
                crate::layers::fqn(crate::layers::layer_of(&n.layer), &n.node_type, &n.name);
            format!(
                "note `{}` is in layer `{}` — a note attaches only to implementation-layer code (global.constraint.note-implementation-only)",
                fqn, n.layer
            )
        })
        .collect()
}

/// The details-shape rule (`global.constraint.note-details-shape`,
/// `domain.constraint.details-canonical-target-set`): each `note`'s `details`
/// edge names exactly one target, and that target is a single **code** identity
/// — the code FQN the note deepens. An authored-node target is off-model: a
/// note target is refused (`global.constraint.note-implementation-only`
/// confines notes to implementation-layer code), and any other authored FQN
/// (`crate::layers::parse_fqn(target).is_ok()`) is off-model too because a
/// `details` edge names a code FQN, never a spec node — the same shape the
/// write surface's `validate_edge` enforces. One error string per violation;
/// pure — no I/O — so the unit tier pins it over in-memory [`NodeFile`]s;
/// [`lint`] feeds it the nodes it read from the durable layer files.
/// `pub(crate)` — the lint's single source of truth for the rule, never a
/// public unit.
pub(crate) fn details_shape_errors(nodes: &[NodeFile]) -> Vec<String> {
    let fqn_of =
        |n: &NodeFile| crate::layers::fqn(crate::layers::layer_of(&n.layer), &n.node_type, &n.name);
    let note_fqns: BTreeSet<String> = nodes
        .iter()
        .filter(|n| n.node_type == "note")
        .map(&fqn_of)
        .collect();

    let mut errors: Vec<String> = Vec::new();
    for n in nodes {
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
                "R4: note `{note}` has {} `details` edge(s) — a note's details edge names exactly one code FQN",
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
                "R4: note `{note}` details another note (`{target}`) — a note's details edge names exactly one code FQN, not another note"
            ));
        } else if crate::layers::parse_fqn(target).is_ok() {
            errors.push(format!(
                "R4: note `{note}` details authored node `{target}` — a note's details edge names exactly one code FQN, not an authored node"
            ));
        }
    }
    errors
}

/// The authored tier-1-3 body budget, in **characters** — never bytes —
/// (`requirements.requirement.spec-body-budget`): one present-tense sentence
/// of at most 244 characters. Single source of truth for the rule's threshold
/// and its message.
pub(crate) const TIER_BODY_BUDGET: usize = 244;

/// The tier-1-3 body-budget rule (`requirements.requirement.spec-body-budget`):
/// every authored body in a tier-1-3 layer (`requirements`/`domain`/`solution`)
/// is one sentence of at most [`TIER_BODY_BUDGET`] **characters** (not bytes),
/// so a body over budget yields one blocking error naming the node, its
/// character count, and the budget.
///
/// The requirement scopes the budget to the tier ("every authored requirement,
/// domain, and solution node body"), not to a node type, and states no
/// exemption. The wording advisory's constraint carve-out is about a
/// constraint's negative *wording* (`global.constraint.spec-timelessness`), not
/// its *length*, so a layer-scoped constraint is included here — a negative
/// rule is still one sentence within the same budget. Pure — no I/O — so the
/// unit tier pins it over in-memory [`NodeFile`]s; [`lint`] feeds it the nodes
/// it read from the durable layer files. `pub(crate)` — the lint's single
/// source of truth for the rule, never a public unit.
pub(crate) fn body_budget_errors(nodes: &[NodeFile]) -> Vec<String> {
    let fqn_of =
        |n: &NodeFile| crate::layers::fqn(crate::layers::layer_of(&n.layer), &n.node_type, &n.name);
    nodes
        .iter()
        .filter(|n| matches!(n.layer.as_str(), "requirements" | "domain" | "solution"))
        .map(|n| (n, n.body.chars().count()))
        .filter(|(_, chars)| *chars > TIER_BODY_BUDGET)
        .map(|(n, chars)| {
            format!(
                "body budget: `{}` body is {chars} characters — an authored tier-1-3 body is one sentence of at most {TIER_BODY_BUDGET} characters (requirements.requirement.spec-body-budget)",
                fqn_of(n)
            )
        })
        .collect()
}

/// The whole-durable-spec deterministic check. Returns `(errors, advisories)`:
/// the blocking rule violations first, the non-blocking tier-1-3 wording
/// advisories second. Read-only — it reads the durable node files and never
/// writes, nor does it read the merge-base tree or the plan store (the
/// change-set delta coverage gates are plan-verification concerns, SPEC §9.3).
///
/// `tier` is the **scope** (SPEC §9.2/§9.4): `Some(layer)` restricts the local
/// per-node rules, the shared mechanical validator set, and the advisories to
/// that tier plus the typed upstream spine paths it requires; `None` checks all
/// tiers. Scope is not context: lint always enforces the ordinary-lint
/// `Context::Lint` boundary (a missing Component `implemented-by` is
/// permitted, SPEC §8.2).
///
/// Errors (blocking — `cmd_spec` reports them and exits non-zero):
/// - **the shared mechanical validator set** — [`model_errors`] runs the SPEC
///   §4 edge-kind matrix ([`validate_edges`]), the event-only
///   `publishes`/`subscribes` target rule ([`validate_event_targets`]) and the
///   whole-model cardinalities ([`validate_cardinalities`]), the SAME functions
///   the write path uses (SPEC §9.1);
/// - **R3** — a node holds at most one note (two notes detailing one target);
/// - **R4** — each note's `details` edge names exactly one **code** identity:
///   the single code FQN the note deepens, never another note and never any
///   authored-node FQN (`global.constraint.note-details-shape`,
///   `domain.constraint.details-canonical-target-set`);
/// - **the note-confinement rule** — a note attaches only to
///   implementation-layer code, so a `note` node in the requirements, domain,
///   solution, or global layer is a violation
///   (`global.constraint.note-implementation-only`);
/// - **the tier-1-3 body budget** — an authored body in the requirements,
///   domain, or solution layer is one sentence of at most
///   [`TIER_BODY_BUDGET`] characters (counted in characters, never bytes), so
///   an over-budget body is a violation
///   (`requirements.requirement.spec-body-budget`).
///
/// Advisories (non-blocking, R1/R5): [`wording_warning`] over every tier-1-3
/// node body except a constraint (a negative rule legitimately lives in a
/// layer-scoped constraint).
pub fn lint(apg_root: &Path, tier: Option<Layer>) -> anyhow::Result<(Vec<String>, Vec<String>)> {
    let nodes = crate::layers::read_existing_nodes(apg_root)?;
    let mut errors: Vec<String> = Vec::new();
    let mut advisories: Vec<String> = Vec::new();

    let fqn_of =
        |n: &NodeFile| crate::layers::fqn(crate::layers::layer_of(&n.layer), &n.node_type, &n.name);

    // Scope: the per-node rules and advisories see only the selected tier's
    // nodes; the shared validator below sees the whole model (an in-scope node's
    // spine path resolves against it) but reports only in-scope subjects.
    let scoped: Vec<NodeFile> = match tier {
        Some(scope) => nodes
            .iter()
            .filter(|n| layer_of(&n.layer) == scope)
            .cloned()
            .collect(),
        None => nodes.clone(),
    };

    // R3 — a node holds at most one note: count the notes detailing each target
    // (a note's `details` out-edge) and report every target with two or more.
    let mut notes_per_target: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for n in &scoped {
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

    // R4 — each note's `details` edge names exactly one **code** identity: the
    // single code FQN the note deepens, never another note and never any
    // authored-node FQN (`global.constraint.note-details-shape`,
    // `domain.constraint.details-canonical-target-set`). The pure helper is the
    // single source of truth, unit-pinned in `src/spec_lint/tests.rs`.
    errors.extend(details_shape_errors(&scoped));

    // The note-confinement rule (`global.constraint.note-implementation-only`)
    // — a note attaches only to implementation-layer code, so a `note` node in
    // the requirements, domain, solution, or global layer is a violation.
    // `read_existing_nodes` reads only durable layer files (plans holds no note
    // type and is never serialized under `apg/layers/`), so any note whose
    // layer is not `implementation` is off-model.
    errors.extend(note_confinement_errors(&scoped));

    // The tier-1-3 body budget (`requirements.requirement.spec-body-budget`) —
    // every authored requirement/domain/solution body is one sentence of at
    // most [`TIER_BODY_BUDGET`] characters (counted in characters). The pure
    // helper is the single source of truth, unit-pinned in
    // `src/spec_lint/tests.rs`.
    errors.extend(body_budget_errors(&scoped));

    // The shared mechanical validator set (SPEC §9.1): the SPEC §4 edge matrix
    // plus the whole-model cardinalities, implemented once in `layers::validate`
    // and shared with the write path. Lint enforces the ordinary-lint context
    // (SPEC §8.2): a missing Component `implemented-by` claim is permitted; a
    // present one is validated. The full model is passed so an in-scope node's
    // upstream spine path resolves even under a single-tier scope.
    errors.extend(model_errors(&nodes, tier, Context::Lint));

    // Advisories — tier-1-3 bodies a reviewer is likely to reject (R1/R5). The
    // shared [`tier_body_warning`] selection exempts a constraint: a negative
    // rule lives legitimately in a layer-scoped constraint.
    for n in &scoped {
        if let Some(msg) = tier_body_warning(&n.layer, &n.node_type, &n.body) {
            advisories.push(format!("{}: {msg}", fqn_of(n)));
        }
    }

    Ok((errors, advisories))
}

/// `apg spec lint` — the argv handler for the `spec` subcommand. The only valid
/// subcommand is `lint`: parse the optional `--tier` scope
/// (`requirements`/`domain`/`solution`; omitted = all tiers, SPEC §9.4),
/// resolve the layout root, run [`lint`], print the non-blocking advisories and
/// then the blocking errors to stderr, and exit non-zero (via the returned
/// error) when any error is found; a clean spec prints a one-line summary to
/// stdout. Unknown flags, a repeated `--tier`, a missing/invalid tier value,
/// and unexpected positional arguments are usage errors. Any other subcommand
/// is a usage error. `pub` so the CLI dispatch in `main` wires it
/// (`rust.apg.main`).
pub fn cmd_spec(args: &[String]) -> anyhow::Result<()> {
    let Some((sub, rest)) = args.split_first() else {
        anyhow::bail!("usage: apg spec lint [--tier requirements|domain|solution]");
    };
    match sub.as_str() {
        "lint" => {
            let tier = parse_lint_args(rest)?;
            let apg_root = crate::plan_cmd::require_apg_root()?;
            let (errors, advisories) = lint(&apg_root, tier)?;
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
        other => anyhow::bail!("usage: apg spec lint (got `{other}`)"),
    }
}

#[cfg(test)]
mod tests;
