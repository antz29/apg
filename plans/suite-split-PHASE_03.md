# suite-split-PHASE_03 — Docs + sync + dogfood verify

References: **suite-split-PLAN.md**, `README.md`, `AGENTS.md`, `~/.opencode/`.
Scope: finish the restructure so the apg repo is a clean dogfood target and the installed
suite is authoritative.

## Deliverable

README/AGENTS name the suite dir; `~/.opencode/` matches the suite exactly (no dead tools, no
drift); `apg init .` runs clean and apg scans/queries its own repo.

## Work items

1. **Docs**: update the "single-sourced from this repo's own `.opencode/`, embedded via
   `include_str!`" statements in `README.md` and `AGENTS.md` to name `opencode-suite/` (also
   fix any `.opencode/tools` path references in prose).
2. **Sync `~/.opencode/`**:
   - Remove the two dead tools `apg_spec_archive.ts` + `apg_spec_promote.ts` (CLI has no
     `archive`/`promote`; init never deletes).
   - Re-run `apg init .` — expect a no-op update line, zero shadow warnings.
3. **Dogfood verify**:
   - `apg scan .` on the repo; `apg query` a few lookups against the repo's graph.
   - Confirm the repo's opencode session loads the tools from `~/.opencode/tools` and the two
     project agents from `.opencode/agents/`.

## Deliverables / done gate

- `apg init .` in the repo prints no shadow warnings and no drift updates.
- `apg scan .` + `apg query "MATCH (m:Module) RETURN m.fqn LIMIT 5"` succeed on the repo.
- `ls ~/.opencode/tools` contains exactly the suite tools (no `apg_spec_archive`/`promote`).
- README + AGENTS reference `opencode-suite/`; `git status` shows the plan docs + doc edits.

## Out of scope

- The 0.10.1 release work already in the tree (README/AGENTS/`--json` help) — commit-ready
  independent of this plan.
- The release ceremony for 0.10.1.