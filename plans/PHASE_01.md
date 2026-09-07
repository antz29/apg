# PHASE_01 — Land the working-tree baseline

References: **HANDOVER.md**, **REVIEW.md**, **0.10.0-PLAN.md**.
Scope: commit the working-tree PHASE_01–05 implementation (built under the **pre-finalization
model** — `Future` with tier-aligned `kind`, invariants, assertion/milestone plan lifecycle,
`future/`-namespace migration, archive/retag removal) as a green, self-contained checkpoint
before the planned-node reconciliation.

## Deliverable

The entire working-tree implementation is committed on `main`: the 4-tier node taxonomy + the
five spine edges, the invariant mechanism, the assertion-only/milestone-only plan lifecycle,
project-scoped FQNs (no `future/` prefix in FQNs), the removal of `apg spec archive` and
`apg plan retag`, docs, and the `invariant-mechanism` dogfood project. `cargo test` green and
clippy clean. Session checkpoint docs (`HANDOVER.md`, `REVIEW.md`, `HANDOFF.md`) stay untracked.

## Work items

1. **Verify the baseline**: `cargo test` + clippy clean at the current working tree; the
   dogfood `invariant-mechanism` spec resolves against a rebuilt graph (spine + `Invariant` +
   delivered requirements); `apg --version` = 0.10.0 (already bumped in the tree).
2. **Commit the checkpoint** (single commit, repo style). Include: `src/*` (all modules incl.
   `invariant_cmd.rs`), `.opencode/` tools + lib + agents, `apg/specs/*.jsonl` (migrated
   project-scoped FQNs + `_invariants.jsonl`), README/AGENTS. **Exclude** the session docs
   (HANDOVER/REVIEW/HANDOFF) from the commit.
3. **Record the known divergence in the commit message**: the baseline implements the
   pre-finalization `Future.kind` re-alignment; the committed specs (`169959d`) mandate planned
   Implementation nodes — reconciled in PHASE_02.

## Deliverables / done gate

- `git log` shows the baseline commit; the tree is clean except the untracked session docs.
- `cargo test` green + clippy clean at the checkpoint.
- `apg scan` on the repo rebuilds a graph where the `invariant-mechanism` dogfood spec resolves
  (spine, `Invariant` via `domain-rule`, delivered requirements).

## Out of scope (later phases)

- The planned-node reconciliation (PHASE_02).
- Closing the REVIEW.md open items (PHASE_03).
- Docs/agents/dogfood re-run under the finalized model (PHASE_04).