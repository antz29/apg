# PHASE_05 — Release readiness (v0.10.0)

References: **0.10.0-PLAN.md**, the apg-0.9.3 release spec (R5/R6 release pattern).
Scope: cut the 0.10.0 release. Code-side version pins were already bumped to 0.10.0 in the
working tree (REVIEW.md item); this phase re-points the release records and queues the
human-owned ceremony.

## Deliverable

The 0.10.0 release is prepared: the final tree green (`cargo test` + clippy clean,
`apg --version` = 0.10.0), the `Formula/*.rb` tag/revision pointers moved to the `v0.10.0` tag,
and the release handoff (tag push, bottle rebuild, formula sha256s) clearly queued for the
human.

## Work items

1. **Final green**: `cargo test` + clippy clean at the post-PHASE_04 state; `apg --version`
   prints 0.10.0; the README/Linux-install prose pins (`apg 0.10.0`/`v0.10.0`) pass the
   release-gate assertions.
2. **Release records**: update `Formula/{scanner,apg-go,apg-java,apg-cpp,apg-rust,apg-ts,
   apg-csharp}.rb` tag/revision pointers (currently at `v0.9.3`) to the `v0.10.0` tag, per the
   apg-0.9.3 R5 release pattern — the version-bump commit precedes the tag + Formula pointer +
   bottle rebuild.
3. **Tag + handoff**: cut the annotated `v0.10.0` tag; write the release handoff (push, bottle
   rebuild, formula sha256s) as human-owned, matching how 0.9.3 was staged.

## Deliverables / done gate

- `cargo test` green; `apg --version` = 0.10.0; README pins at 0.10.0.
- Formulas point at the `v0.10.0` tag (pointer-only — the bottle rebuild + sha256 update is
  human-owned at tag time).
- Release handoff documented for the human.

## Out of scope (human-owned)

- The release ceremony itself: pushing the tag, rebuilding bottles, updating formula sha256s.
- The 1.0.0 Cosanima release (its own spec).