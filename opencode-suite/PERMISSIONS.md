# OpenCode permission model

This guide records the permission model the apg opencode suite is written
against: how an agent's grants are declared, how one call is decided, and where
each part lives in this repo. The rules are data, not code — every agent
declares its own ordered `permissions` array in its frontmatter, and opencode's
permission engine evaluates it.

## Deny by default

An action with no matching allow is refused. Every rule list opens with a
catch-all deny, `{action: "*", resource: "*", effect: "deny"}`, so a tool or
path is reachable only because a later rule grants it. `effect` is one of
`allow`, `deny`, or `ask`; an unmatched resource is refused, never allowed.

## Rule shape and ordering

Each grant is `{action, resource, effect}`. The array is ordered and the
**last matching rule wins**, so a later rule overrides an earlier one for the
same resource. An allow followed by a narrower deny revokes that path; a broad
allow followed by specific denies carves out exceptions. The order therefore
carries meaning and must be preserved when an agent is scaffolded or edited.

## Whole-value pattern matching

A pattern is matched against a resource's **whole value**, not searched for
inside it. `*` matches any run of characters, including `/`; `?` matches
exactly one character. The pattern vocabulary is `*` and `?`: there is no
separate `**` recursive form (a `**` is simply two `*`), no brace expansion,
and no character-class syntax. A resource must be covered end to end for the
rule to match.

## The one-check reduction

A single check reduces the outcome of one assert call: **any matching deny →
deny**; otherwise **any matching ask → ask**; otherwise **allow**. The
reduction runs across the resources of that one call. It is never assembled by
combining separate rule lists or separate calls: each resource's winning effect
comes from the ordered rule list (last match), and the deny/ask/allow reduction
is applied to the winners of the resources in the one assert.

## Resource framing and one assert per operation

Resources are asserted in the **main-anchored frame**: a canonical absolute
path is mapped to the bare, `/`-separated path relative to the main checkout
root — no leading `/`, matching the spelling an agent's globs use (for example
`src/a.rs` or `apg/.worktrees/<project>/x.txt`). A path equal to the root,
outside it, or escaping it stays **absolute**, so a genuinely external path is
asserted against the absolute rules. The mapping runs only after the path is
canonicalised, and the frame root is the main checkout, so every candidate and
root are compared in one symlink-resolved frame.

An operation asserts **all** of its resources in **one** `permission.assert`
call: source and destination together for a move/copy, every path at once for a
removal. A write whose source is allowed but whose destination is denied is
refused as a whole. Refusals are fail-closed: a missing permission surface or
an unresolvable frame root refuses before any filesystem work.

## Gate 1 and gate 2

Two gates stand between a call and its effect:

- **Gate 1** is the tool's own action. A tool call is reachable only when the
  agent's rule list admits that action and the call passes
  `Tool.Options.permission`; a denied tool never runs.
- **Gate 2** is the plugin's per-path write assertion. The filesystem tools
  call `fsScopeDecision`, which asserts the `edit` action over the same
  main-anchored paths. A file write needs both gates: gate 1 admits the tool,
  gate 2 admits the paths. Gate 1 alone does not establish that a particular
  path is writable.

## The read guard

Graph state is readable only through the apg tools. Every distributed and
generated agent allows `read`, `glob`, and `grep` broadly and then denies the
graph-state paths: `apg/.trans/**` and `apg/layers/**`, plus their worktree
mirrors `apg/.worktrees/*/apg/.trans/**` and
`apg/.worktrees/*/apg/layers/**`. Because the last matching rule wins, those
denies override the broad allow. The guard covers the transient plan, feedback,
and database files as well as the durable node files; neither is read directly.

## The shell allowlist

The `shell` action is deny-by-default. It starts with a `*` deny, followed by
exact command patterns such as `ls *`, `git status *`, or `scripts/gate.sh *`.
One command runs per call: a command containing `&&`, `|`, `;`, `$()`, or
redirection matches no pattern and is refused. Patterns match the whole command
string, so an env-prefixed form such as `FOO=1 cargo build` no longer matches
the `cargo build *` pattern and is refused unless that exact env-prefixed
string is granted. The suite's single deliberate env-prefixed grant is
`APG_SUITE_E2E=1 bun test tests/boundary.e2e.test.ts`, held by the
agent-builder template alone.

## The scratch markdown grant

An agent's `edit` grant is what admits `edit`, `write`, and `patch` — `edit` is
the action that covers all three, so one rule list governs every file write.
The navigator's and the researcher's `edit` grant is a deny-base followed by
exactly two allows:

- `apg/.scratch/*.md`
- `apg/.worktrees/*/apg/.scratch/*.md`

The rules are byte-identical to the templates, and every other path is refused.
The `.md` suffix matches the **whole** value, so `apg/.scratch/notes.txt` and
`apg/.scratch/sub/x.md` match neither pattern and are refused — see *Whole-value
pattern matching* above and
`requirements.constraint.scratch-markdown-only-writes`. Gate 1 is the `edit`
action rule set: a native `edit`/`write`/`patch` scratch write is admitted by
those rules alone. Gate 2 (`fsScopeDecision`) re-asserts the same main-anchored
paths through that same `edit` rule set, and runs only when the write goes
through the suite's filesystem tools (`apg_rm`, `apg_mv`, `apg_cp`), which call
it themselves. Neither the navigator nor the researcher holds those tools, so
for both the `edit` rules decide every scratch write. The navigator holds the
same grant because it dispatches the researcher and reads the markdown it
produces.

## `external_directory`

`external_directory` gates paths outside the workspace root. Two shapes exist,
and which one an agent carries is part of its grant.

The generated agents — every scaffolded `*-implementer` and the rest — and the
`codebase-navigator` carry exactly two rules: `{resource: "*", effect: "deny"}`
followed by `{resource: "/tmp/**", effect: "allow"}`. Every generated agent
carries this shape, and for them a broader external grant is a regression, not
an option.

The researcher carries `{resource: "*", effect: "deny"}` followed by
`{resource: "*", effect: "ask"}` — **no external allow exception at all**. Asking
for every external path is **tighter** than the navigator's `/tmp/**` allow: it
pre-grants nothing and surfaces each decision, so this shape is **not** a
relaxation of the regression law above. The researcher is the one agent that
asks rather than holding a `/tmp` allow because one research question can need an
arbitrary external source, so the decision is put to the caller per path instead
of pre-granted.

## The web actions

The two web actions are `websearch`, which matches the search query, and
`webfetch`, which matches the requested URL. Both are
`{resource: "*", effect: "allow"}`, and only the researcher template declares
them. The spellings are resolved from the OpenCode V2 permissions reference —
https://opencode.ai/v2/docs/permissions, its *Actions* table — not inferred: a
guessed or V1 spelling would silently deny the action (or degrade it to `ask`) at
runtime instead of failing loudly.

## Refusals

A gate-2 refusal carries the reason from the rejected `permission.assert`; when
no reason is available, the repo-verifiable fallback literal is
`permission denied`. Do not assume any other message format — the fallback is
defined in `assertFailureReason` in `opencode-suite/lib/apg.ts` (line 446), and
the filesystem tools throw `permission denied` when the decision has no reason.
The assert precedes the filesystem operation, so a refused operation changes
nothing.

## Where this lives in the code

- `opencode-suite/lib/apg.ts` — `mainAnchoredResource` (the main-anchored
  mapping), `fsScopeDecision` (one-assert operation, frame resolution,
  fail-closed reasons), and `assertFailureReason` (the refusal reason and the
  `permission denied` fallback).
- `opencode-suite/tools/apg_rm.ts`, `apg_mv.ts`, `apg_cp.ts` — the filesystem
  operations; each resolves its paths, calls `fsScopeDecision` once with all of
  them, and throws before touching the filesystem when refused.
- `opencode-suite/agents/agent-builder.md` — the scaffold rules: ordered
  deny-first grants, the gate 1/gate 2 explanation, the read guard, the shell
  policy (including the env-prefix rule), and `external_directory`.
- `opencode-suite/agents/researcher.md` — the deny-then-ask
  `external_directory` shape (the stricter external variant), the two scratch
  markdown `edit` allows, and the `websearch`/`webfetch` grants.
- The remaining `opencode-suite/agents/*.md` templates — each declares its own
  ordered `permissions` array and restates the model in its `## Permissions`
  section, including the read guard and its shell allowlist.
