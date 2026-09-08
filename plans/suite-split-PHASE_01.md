# suite-split-PHASE_01 — Move the suite + re-point embeds

References: **suite-split-PLAN.md**, `src/main.rs`, `.opencode/`.
Scope: physically separate the install template from the repo's live opencode dir.

## Deliverable

The template lives in `opencode-suite/` (tracked): `tools/*.ts` (46), `lib/apg.ts`,
`agents/{codebase-navigator,spec-writer,plan-writer,spec-review,plan-review,agent-builder}.md`.
`src/main.rs` embeds from the suite dir. The repo's `.opencode/` contains only the two
project-specific agents. The binary still builds green.

## Work items

1. **Create `opencode-suite/` and `git mv` the template** (preserving the relative layout so
   tool `../lib/apg.ts` imports resolve):
   - `git mv .opencode/tools .opencode/lib .opencode/agents/{codebase-navigator,spec-writer,plan-writer,spec-review,plan-review,agent-builder}.md opencode-suite/`
   - Add `opencode-suite/.gitignore` (node_modules, package.json, package-lock.json, bun.lock).
   - Add a local `opencode-suite/package.json` (`@opencode-ai/plugin` pin, matching the embedded
     `OPENCODE_PACKAGE_JSON`) + `npm install` so direct tool imports (smoke test, dev) resolve.
2. **Re-point the `include_str!` embeds** in `src/main.rs` from `../.opencode/…` to
   `../opencode-suite/…` — all `SUITE_TOOLS` entries (30-223), `APG_LIB` (229), `AGENTS`
   (236-261) — and the "six distributed agents" doc comment.
3. **Shrink the repo's `.opencode/`** to project-only:
   - Keep `agents/{implementer,implementation-phase-reviewer}.md`.
   - Remove the now-empty `tools/`, `lib/`, the moved agents, and the untracked
     `node_modules/` + `package.json` + `package-lock.json` + stale `.opencode/.gitignore`.

## Deliverables / done gate

- `git status` shows the suite files moved (renames detected), `.opencode/` holds only the two
  project agents.
- `cargo build` succeeds with the re-pointed embeds.
- `cargo test` green; clippy clean.
- The two project agents still load with the repo's opencode session (tools from
  `~/.opencode`, already installed).

## Out of scope (later phases)

- The shadow-check walk fix and smoke-test path update (PHASE_02).
- Docs, `~/.opencode` sync, and the in-repo dogfood verify (PHASE_03).