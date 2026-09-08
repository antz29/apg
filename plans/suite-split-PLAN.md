# suite-split-PLAN.md — split the opencode install template out of `.opencode/`

Status: working plan (files only). No release-version target — a repo-internal
restructure so the apg repo dogfoods itself cleanly (the 0.10.1 README/AGENTS/`--json`
changes are already in the tree and independent of this plan).

## Context

The repo's `.opencode/` is doing double duty:

1. **Tracked install template** — 46 tools + `lib/apg.ts` + the six distributed agents,
   embedded into the binary via `include_str!` (`src/main.rs:30-261`) and written to
   `~/.opencode/` by `apg init`.
2. **The apg repo's own live opencode dir** — the same files, plus 2 project-specific agents
   (`implementer`, `implementation-phase-reviewer`) and a gitignored `node_modules/` so the
   tools' `@opencode-ai/plugin` import resolves.

That conflation breaks dogfooding: `apg init .` in the repo hits `duplicate_install_files`
(`src/main.rs:616`), which walks `.opencode/` recursively and flags every file that also exists
in `~/.opencode/` — the 53 suite files **plus every shared `node_modules` file** (the walk does
not skip node_modules), producing a wall of shadow warnings. Because the suite is a tracked
template *and* a live config, the two roles can't be separated by editing either one.

## Objective

- The install template lives in a **dedicated, tracked suite directory** (`opencode-suite/`),
  distinct from the project `.opencode/`.
- The repo's `.opencode/` holds **only project-specific agents** (a normal apg-managed
  project); tools come from `~/.opencode` after `apg init`.
- `apg init .` in the apg repo runs **clean** (no shadow wall), and apg can scan/query its own
  repo — the dogfood loop works against the real install path.
- The shadow check stops flagging `node_modules` noise in any project.
- **`apg init` prunes stale apg-owned files** from `~/.opencode/` (a tool/agent
  removed from the suite in a newer release is deleted on the next init, not left
  to shadow forever) while preserving anything that isn't apg-owned.
- `~/.opencode` is back in sync with the suite (no dead tools, no drift).

## Strategy

Additive and independently verifiable:

1. **Move the template** into `opencode-suite/` (git mv, layout preserved so `../lib/apg.ts`
   imports keep resolving); re-point the `include_str!` embeds.
2. **Fix the walk + test paths** — shadow check skips `node_modules`; the invariant smoke test
   imports from the suite dir.
3. **Docs + user-level sync + dogfood verify** — README/AGENTS name the suite dir; drop the two
   dead `~/.opencode` tools; `apg init .` clean, `apg scan`/`apg query` against the repo.

## Phase table

| Phase | Title | Deliverable |
|---|---|---|
| 01 | **Move the suite + re-point embeds** | `opencode-suite/` holds tools/lib/agents (tracked); `src/main.rs` embeds it; repo `.opencode/` shrinks to the two project agents |
| 02 | **Init hygiene** | `duplicate_install_files` skips node_modules/lockfiles; `cmd_init` prunes stale apg-owned files from `~/.opencode/` (tools/agents removed from the suite); invariant smoke test imports from the suite dir |
| 03 | **Docs + sync + dogfood verify** | README/AGENTS reference `opencode-suite/`; `~/.opencode` dead tools removed and resynced; `apg init .` clean + `apg scan`/`apg query` on the repo green |

See **suite-split-PHASE_01.md … suite-split-PHASE_03.md** for detailed work items and done
gates.

## Reference files

- `src/main.rs` — `SUITE_TOOLS`/`APG_LIB`/`AGENTS` `include_str!` embeds (30-261);
  `user_opencode_dir` (591); `cmd_init` (646); `duplicate_install_files` (616).
- `src/invariant_cmd.rs` — suite smoke test imports (848-850).
- `.opencode/` — current template + project agents + gitignored node_modules.
- `README.md` / `AGENTS.md` — "single-sourced from this repo's own `.opencode/`" statements.
- `~/.opencode/` — the installed suite; `apg_spec_archive.ts` + `apg_spec_promote.ts` are dead
  leftovers (CLI has no `archive`/`promote`).

## Decisions

- Suite dir name: **`opencode-suite/`** at the repo root (changeable in PHASE_01 if a better
  name wins).
- Install target stays `~/.opencode`; the embedding mechanism (`include_str!`) is unchanged —
  only the source path moves.
- The repo keeps its native self-hosting *via the installed suite* (tools from `~/.opencode`),
  not from a project-local copy — the honest install-path dogfood.

## Out of scope

- The 0.10.1 release work already in the tree (README `--json`/versioning, AGENTS.md fixes,
  `src/main.rs` help line) — independent, already committed-ready.
- Changing how opencode discovers tools (no `tools.paths` config mechanism exists; the suite
  dir is *not* a live opencode dir).
- Moving the install target from `~/.opencode` to `~/.config/opencode`.
- The release ceremony (tag/bottle/sha256) for 0.10.1.