// Unit tests for the shared suite plumbing (`opencode-suite/lib/apg.ts`).
//
// Runner: `bun test` (opencode-suite is a Bun package — `apg.ts` uses `Bun.$`
// in `runCypher`/`runCli`). Only the side-effect-free helpers are exercised
// here; the subprocess/fs paths are covered by the opt-in boundary e2e.
import { test, expect } from "bun:test"

import {
  type ToolContext,
  lit,
  csvToRows,
  projectOf,
  codeTypeCondition,
  noteIfEmpty,
  isQueryError,
  expectQueryOk,
  mainAnchoredResource,
  resolveProjectPath,
  findSymbolRebaseColumns,
  scopeProjectRequirements,
  fsScopeDecision,
  REQUIREMENT_FQN_PREFIX,
  NO_DB_ERROR,
  QUERY_FAILED_PREFIX,
} from "./apg.ts"

test("lit single-quotes and escapes \\ ' \" and newlines", () => {
  expect(lit("plain")).toBe("'plain'")
  expect(lit("a'b")).toBe("'a\\'b'")
  expect(lit('a"b')).toBe("'a\\\"b'")
  expect(lit("a\\b")).toBe("'a\\\\b'")
  expect(lit("a\nb")).toBe("'a\\nb'")
  expect(lit("a\rb")).toBe("'a\\rb'")
})

test("codeTypeCondition is empty for all/default and a quoted predicate otherwise", () => {
  expect(codeTypeCondition("n")).toBe("")
  expect(codeTypeCondition("n", "all")).toBe("")
  expect(codeTypeCondition("n", "test")).toBe("n.code_type = 'test'")
})

test("projectOf extracts the project prefix from a plan-family FQN", () => {
  expect(projectOf("proj/plan.phase-01")).toBe("proj")
  expect(projectOf("proj/feedback-1")).toBe("proj")
  expect(projectOf("requirements.requirement.x")).toBe(null)
})

test("csvToRows parses a header + rows with quoted fields", () => {
  expect(csvToRows("a,b\n1,2\n")).toEqual([
    ["a", "b"],
    ["1", "2"],
  ])
  expect(csvToRows('a,b\n"x,y",z\n')).toEqual([
    ["a", "b"],
    ["x,y", "z"],
  ])
  expect(csvToRows('a\n"he said ""hi"""\n')).toEqual([["a"], ['he said "hi"']])
})

test("isQueryError / expectQueryOk guard the two runCypher failure signals", () => {
  expect(isQueryError(NO_DB_ERROR)).toBe(true)
  expect(isQueryError(`${QUERY_FAILED_PREFIX} (exit 1):boom`)).toBe(true)
  expect(isQueryError("a,b\n1,2")).toBe(false)
  expect(() => expectQueryOk(NO_DB_ERROR)).toThrow()
  expect(expectQueryOk("a\n1")).toBe("a\n1")
})

test("noteIfEmpty appends only when there are no data rows", () => {
  expect(noteIfEmpty("headers\n", "(none)")).toBe("headers\n\n(none)")
  expect(noteIfEmpty("headers\nrow\n", "(none)")).toBe("headers\nrow\n")
})

// fix-module-identity task-27: the pure stored<->absolute conversion. No fs,
// no subprocess, no apg spawn — `resolveProjectPath` is a path spelling
// transform only.
test("resolveProjectPath maps a stored repo-relative identity to an absolute path", () => {
  const dir = "/home/u/repo"
  expect(resolveProjectPath(dir, "src/a.rs")).toBe("/home/u/repo/src/a.rs")
  expect(resolveProjectPath(dir, "pkg/sub/b.go")).toBe("/home/u/repo/pkg/sub/b.go")
})

test("resolveProjectPath recognizes an already-absolute input and maps it back to the stored value", () => {
  const dir = "/home/u/repo"
  expect(resolveProjectPath(dir, "/home/u/repo/src/a.rs")).toBe("src/a.rs")
  // An absolute path outside the project dir has no stored spelling: unchanged.
  expect(resolveProjectPath(dir, "/elsewhere/a.rs")).toBe("/elsewhere/a.rs")
  // The empty cell (an absent path column) passes through.
  expect(resolveProjectPath(dir, "")).toBe("")
})

test("resolveProjectPath round-trips in both directions", () => {
  const dir = "/home/u/repo"
  const stored = "src/deep/a.rs"
  const absolute = "/home/u/repo/src/deep/a.rs"
  expect(resolveProjectPath(dir, resolveProjectPath(dir, stored))).toBe(stored)
  expect(resolveProjectPath(dir, resolveProjectPath(dir, absolute))).toBe(absolute)
})

// fix-module-identity feedback-17: `apg_find_symbol` is the one curated tool
// whose identity cell is kind-dependent — a File row's fqn (column 1) is its
// path, a symbol row's fqn is a symbol name and its path (column 2) is the
// path. The selector must pick the identity cell per kind, never blind-rebase
// column 1 (which would join a symbol name under the project dir).
test("findSymbolRebaseColumns rebases a File's fqn but a symbol's path", () => {
  // kind, n.fqn, n.path, start, end
  expect(findSymbolRebaseColumns(["File", "src/a.rs", "", "1", "9"])).toEqual([1, 2])
  expect(findSymbolRebaseColumns(["Struct", "pkg.A", "src/a.go", "1", "5"])).toEqual([2])
  expect(findSymbolRebaseColumns(["Function", "pkg.Leaf", "src/a.go", "7", "9"])).toEqual([2])
  // Module/UnresolvedTarget carry neither cell; the path column is a no-op.
  expect(findSymbolRebaseColumns(["Module", "go.example/repo", "", "", ""])).toEqual([2])
})

// The failure this fixes: a blind column rebase of a symbol fqn would turn
// `pkg.A` into `<projectDir>/pkg.A` (a path that does not exist). The selector
// must never rebase column 1 for a non-File row.
test("findSymbolRebaseColumns does not turn a symbol fqn into a path", () => {
  const symbol = ["Struct", "pkg.A", "pkg/a.go", "1", "5"]
  expect(findSymbolRebaseColumns(symbol)).not.toContain(1)
  const file = ["File", "pkg/a.go", "", "1", "9"]
  expect(findSymbolRebaseColumns(file)).toContain(1)
})

// apg-plan-phases-scoping phase-01: the PURE project-requirement scoping core.
// `scopeProjectRequirements` is the scoping DECISION — side-effect-free (no fs,
// no subprocess, no db, no git), so it lives in the `bun test` unit tier; the
// git branch-delta wrapper that feeds it is covered by the opt-in boundary
// e2e. `satisfies` is THIS project's `Satisfies` relation (requirement FQN ->
// this project's phase FQNs): a foreign project's edge is filtered by the
// caller and never reaches the core.
const REQ = REQUIREMENT_FQN_PREFIX

test("scopes a branch-added-only requirement and reports it unsatisfied", () => {
  const scope = scopeProjectRequirements([`${REQ}added`], ["added"], new Map())
  expect(scope.inScope).toEqual([`${REQ}added`])
  expect(scope.unsatisfied).toEqual([`${REQ}added`])
  expect(scope.overSatisfied).toEqual([])
})

test("scopes a satisfied-only requirement with no unsatisfied finding", () => {
  const scope = scopeProjectRequirements(
    [`${REQ}done`],
    [],
    new Map([[`${REQ}done`, ["proj/plan.phase-01"]]]),
  )
  expect(scope.inScope).toEqual([`${REQ}done`])
  expect(scope.unsatisfied).toEqual([])
  expect(scope.overSatisfied).toEqual([])
})

test("unions branch-added and this project's Satisfied requirements in graph order", () => {
  const scope = scopeProjectRequirements(
    [`${REQ}added`, `${REQ}done`],
    ["added"],
    new Map([[`${REQ}done`, ["proj/plan.phase-01"]]]),
  )
  expect(scope.inScope).toEqual([`${REQ}added`, `${REQ}done`])
  expect(scope.unsatisfied).toEqual([`${REQ}added`])
  expect(scope.overSatisfied).toEqual([])
})

test("reports no in-scope requirements when nothing is branch-added or satisfied", () => {
  const scope = scopeProjectRequirements([`${REQ}other`], [], new Map())
  expect(scope.inScope).toEqual([])
  expect(scope.unsatisfied).toEqual([])
  expect(scope.overSatisfied).toEqual([])
})

test("an in-scope branch-added requirement with no Satisfies is unsatisfied", () => {
  const scope = scopeProjectRequirements(
    [`${REQ}added`, `${REQ}done`],
    ["added"],
    new Map([[`${REQ}done`, ["proj/plan.phase-01"]]]),
  )
  expect(scope.unsatisfied).toEqual([`${REQ}added`])
})

test("reports a requirement Satisfied by two phases of this project as over-satisfied", () => {
  const scope = scopeProjectRequirements(
    [`${REQ}twice`],
    [],
    new Map([[`${REQ}twice`, ["proj/plan.phase-01", "proj/plan.phase-02"]]]),
  )
  expect(scope.overSatisfied).toEqual([
    { requirement: `${REQ}twice`, phases: ["proj/plan.phase-01", "proj/plan.phase-02"] },
  ])
  expect(scope.unsatisfied).toEqual([])
})

test("a requirement Satisfied by another project's phase is not over-satisfied by this project", () => {
  // `b` is also Satisfied by `other/plan.phase-01` in the raw relation; the
  // caller filters the map to THIS project's phases, so `b` has exactly one
  // satisfying phase here and must not read over-satisfied.
  const single = scopeProjectRequirements(
    [`${REQ}b`],
    [],
    new Map([[`${REQ}b`, ["proj/plan.phase-01"]]]),
  )
  expect(single.overSatisfied).toEqual([])
  expect(single.inScope).toEqual([`${REQ}b`])
  // A requirement delivered by another project ONLY (absent from this
  // project's Satisfies) is outside the scope entirely — the bug this fixes.
  const foreign = scopeProjectRequirements([`${REQ}c`], [], new Map())
  expect(foreign.inScope).toEqual([])
  expect(foreign.overSatisfied).toEqual([])
  expect(foreign.unsatisfied).toEqual([])
})

// opencode-2x-support phase-01.task-5: the V2 fs-scope decision. `fsScopeDecision`
// no longer re-derives an agent's edit grants (the retired `editAllowGlobs`
// parser): it asserts an operation's resolved path(s) through opencode 2.x's
// permission engine (`ctx.permission.assert`, the `edit` action) and reduces the
// outcome to `{ allowed, reason }`.
//
// TIER: in production the decision's frame does real I/O — `mainCheckoutRoot`
// shells `git rev-parse` and `canonicalPath` calls `realpathSync` — so the REAL
// frame is covered only by the opt-in boundary e2e. Here the same decision is
// driven with the injected PURE frame (`pureFrame`: a fixed root resolver plus an
// identity canonicaliser) and a stubbed `permission.assert`, which makes this
// file's `fsScopeDecision` coverage INT — two units wired (the decision over
// `mainAnchoredResource`, plus the rule-set stub) — kept pure precisely by the
// seam. `mainAnchoredResource` alone is the pure unit tier.
//
// `permission.assert` is never imported from `@opencode/plugin` here: it is a
// structurally-typed `{ assert }` seam the tools pass through, so a stub models
// opencode's decision (resolve = allow, reject = deny) with no live opencode 2.x.

/** A faked opencode 2.x tool-executor context — the V2 shape
 *  `{ agent, sessionID, messageID, id }` (no V1 `directory`/`worktree`). */
function v2Context(overrides: Partial<ToolContext> = {}): ToolContext {
  return { agent: "implementer", sessionID: "s1", messageID: "m1", id: "c1", ...overrides }
}

/** The pure frame `fsScopeDecision` takes as its injectable seam: a fixed root
 *  resolver plus an identity canonicaliser (its inputs are already canonical).
 *  The real default resolves the root with `git rev-parse` and canonicalises
 *  through `realpathSync` — real I/O — so injecting this stand-in is what keeps
 *  these int-tier cases pure. */
const FIXED_ROOT = "/repo"
const pureFrame = {
  resolveRoot: async () => FIXED_ROOT,
  canonicalise: (p: string) => p,
}

/** A stubbed `ctx.permission` whose single `assert` either resolves (allow) or
 *  throws/rejects the given error (deny), capturing every args object it was
 *  called with so a test can assert on the constructed call. */
function stubPermission(reject?: unknown): {
  permission: { assert: (args: unknown) => Promise<void> }
  calls: unknown[]
} {
  const calls: unknown[] = []
  const permission = {
    assert: async (args: unknown): Promise<void> => {
      calls.push(args)
      if (reject !== undefined) throw reject
    },
  }
  return { permission, calls }
}

/** An opencode edit rule: `{ action, resource, effect }`, evaluated in order. */
interface ScopeRule {
  action: string
  resource: string
  effect: "allow" | "deny" | "ask"
}

/** Whole-resource glob match: `*` spans `/` (zero or more), `?` is exactly one
 *  character; backslashes normalise to `/`. No prefix or substring magic — the
 *  pattern must match the ENTIRE resource. */
function scopeRuleMatches(pattern: string, resource: string): boolean {
  const source = pattern
    .replace(/\\/g, "/")
    .replace(/[.+^${}()|[\]\\]/g, "\\$&")
    .replace(/\*/g, ".*")
    .replace(/\?/g, ".")
  return new RegExp(`^${source}$`).test(resource.replace(/\\/g, "/"))
}

/** A stubbed `ctx.permission` that models opencode's edit decision over an
 *  ORDERED rule list in TWO stages: per resource the LAST rule whose action AND
 *  resource patterns both match (whole-string, `*`/`?` wildcards) wins and no
 *  match falls back to `ask` (never allow), then across the resources of ONE
 *  call any `deny` -> deny, else any `ask` -> ask, else allow. A later specific
 *  `allow` therefore overrides an earlier broad `*` `deny` — the any-deny
 *  precedence exists only across resources, never across rules. `allow`
 *  resolves; `deny`/`ask` throw an Error naming the offending resource so
 *  `fsScopeDecision` reduces it to `{ allowed: false, reason }`. */
function scopeRulePermission(rules: readonly ScopeRule[]): {
  permission: { assert: (args: unknown) => Promise<void> }
  calls: unknown[]
} {
  const calls: unknown[] = []
  const permission = {
    assert: async (args: unknown): Promise<void> => {
      calls.push(args)
      const { action, resources } = args as { action: string; resources: readonly string[] }
      // Stage 1 — per resource, the last rule whose action and resource
      // patterns both match wins.
      const effects = resources.map((resource) => {
        let effect: ScopeRule["effect"] = "ask"
        for (const rule of rules) {
          if (scopeRuleMatches(rule.action, action) && scopeRuleMatches(rule.resource, resource)) effect = rule.effect
        }
        return effect
      })
      // Stage 2 — across this call's resources only.
      const refusal = effects.includes("deny") ? "deny" : effects.includes("ask") ? "ask" : null
      if (refusal === null) return
      const hit = resources[effects.indexOf(refusal)]
      throw new Error(refusal === "deny" ? `path denied: ${hit}` : `path asks for approval: ${hit}`)
    },
  }
  return { permission, calls }
}

// ORDERED RULES — the two-stage edit decision `scopeRulePermission` models.

test("scopeRulePermission lets a later worktree allow override the broad `*` deny and rejects its absolute spelling", async () => {
  // This repo's core-implementer grant shape: the real base policy allows
  // everything, then the agent denies all edits and allows the worktree globs.
  // The later, more specific allow must win — a deny-wins-over-the-rule-list
  // implementation refuses the worktree path here.
  const { permission, calls } = scopeRulePermission([
    { action: "*", resource: "*", effect: "allow" },
    { action: "edit", resource: "*", effect: "deny" },
    { action: "edit", resource: "apg/.worktrees/*/**", effect: "allow" },
  ])
  const anchored = "apg/.worktrees/fs-scope-frame/opencode-suite/lib/apg.test.ts"
  await permission.assert({ action: "edit", resources: [anchored] })
  expect(calls).toHaveLength(1)

  // The SAME file in its absolute spelling matches neither pattern as written
  // (the allow is worktree-relative), so it falls through to the base allow and
  // the broad edit deny — main-anchoring the resource is what makes the
  // override possible.
  await expect(
    permission.assert({ action: "edit", resources: [`/repo/${anchored}`] }),
  ).rejects.toThrow(`path denied: /repo/${anchored}`)

  // A resource matched ONLY by the base rule resolves: no edit rule matches
  // `read`, so the base `*`/`*` allow decides — exact action equality would
  // fall back to `ask`.
  await permission.assert({ action: "read", resources: ["docs/readme.txt"] })
})

test("scopeRulePermission asks on no match and refuses a call when ANY resource is denied", async () => {
  // No matching rule is `ask` — never allow.
  const noMatch = scopeRulePermission([{ action: "edit", resource: "src/**", effect: "allow" }])
  await expect(
    noMatch.permission.assert({ action: "edit", resources: ["apg/notes.md"] }),
  ).rejects.toThrow("path asks for approval: apg/notes.md")

  // Stage 2 is across the resources of ONE call: an allowed source plus a denied
  // destination refuses the whole operation, even though the two verdicts come
  // from different rules and the broad deny comes FIRST in the list.
  const { permission } = scopeRulePermission([
    { action: "edit", resource: "*", effect: "deny" },
    { action: "edit", resource: "src/**", effect: "allow" },
  ])
  await expect(
    permission.assert({ action: "edit", resources: ["src/a.rs", "outside/b.rs"] }),
  ).rejects.toThrow("path denied: outside/b.rs")
  await permission.assert({ action: "edit", resources: ["src/a.rs", "src/b.rs"] })
})

// FAIL-CLOSED — no permission surface, or one whose `assert` is not a function.

test("fsScopeDecision fail-closes when the permission surface is absent", async () => {
  const ctx = v2Context()
  const missing = await fsScopeDecision(undefined, ctx, ["/repo/src/a.rs"])
  expect(missing.allowed).toBe(false)
  expect(missing.reason).toContain("opencode permission surface unavailable")
})

test("fsScopeDecision fail-closes when permission.assert is undefined", async () => {
  const noAssert = {
    assert: undefined as unknown as (args: unknown) => Promise<void>,
  }
  const d = await fsScopeDecision(noAssert, v2Context(), ["/repo/src/a.rs"])
  expect(d.allowed).toBe(false)
  expect(d.reason).toContain("opencode permission surface unavailable")
})

// ALLOW / DENY — the assert outcome reduced to the decision. Every case injects
// `pureFrame`, so no case reaches the real frame's fs/git I/O.

test("fsScopeDecision allows when permission.assert resolves", async () => {
  const { permission } = stubPermission()
  const d = await fsScopeDecision(permission, v2Context(), ["/repo/src/a.rs"], undefined, pureFrame)
  expect(d.allowed).toBe(true)
  expect(d.reason).toBe(null)
})

test("fsScopeDecision refuses with the thrown error's message", async () => {
  const { permission } = stubPermission(new Error("path is outside the granted scope"))
  const d = await fsScopeDecision(permission, v2Context(), ["/repo/src/a.rs"], undefined, pureFrame)
  expect(d.allowed).toBe(false)
  expect(d.reason).toBe("path is outside the granted scope")
})

test("fsScopeDecision extracts reason from a rejection carrying only a reason field", async () => {
  // A rejected permission prompt surfaces as a Tool.Error-shaped object whose
  // refusal lives on `reason`, not `message`.
  const { permission } = stubPermission({ reason: "denied by the user prompt" })
  const d = await fsScopeDecision(permission, v2Context(), ["/repo/src/a.rs"], undefined, pureFrame)
  expect(d.allowed).toBe(false)
  expect(d.reason).toBe("denied by the user prompt")
})

test("fsScopeDecision refuses a rejected promise (async deny)", async () => {
  const permission = {
    assert: (_args: unknown): Promise<void> => Promise.reject(new Error("async deny")),
  }
  const d = await fsScopeDecision(permission, v2Context(), ["/repo/src/a.rs"], undefined, pureFrame)
  expect(d.allowed).toBe(false)
  expect(d.reason).toBe("async deny")
})

// ASSERT-ARGS CONSTRUCTION — the call built from the V2 context.

test("fsScopeDecision builds the assert args from the V2 context", async () => {
  const ctx = v2Context({ agent: "implementer", sessionID: "s1", messageID: "m1", id: "c1" })
  const { permission, calls } = stubPermission()
  await fsScopeDecision(
    permission,
    ctx,
    ["/repo/src/a.rs", "/repo/src/b.rs"],
    undefined,
    pureFrame,
  )

  expect(calls).toHaveLength(1)
  const args = calls[0] as Record<string, unknown>
  expect(args.action).toBe("edit")
  // The resources the frame rebuilds are MAIN-ANCHORED, not the absolute inputs.
  expect(args.resources).toEqual(["src/a.rs", "src/b.rs"])
  expect(args.sessionID).toBe("s1")
  const source = args.source as Record<string, unknown>
  expect(source.type).toBe("tool")
  expect(source.messageID).toBe("m1")
  expect(source.id).toBe("c1")
  expect(args.agent).toBe("implementer")
  expect("save" in args).toBe(false)
})

test("fsScopeDecision omits agent when the context agent is empty", async () => {
  // An absent agent lets opencode default to the session's agent.
  const { permission, calls } = stubPermission()
  await fsScopeDecision(permission, v2Context({ agent: "" }), ["/repo/src/a.rs"], undefined, pureFrame)
  const args = calls[0] as Record<string, unknown>
  expect("agent" in args).toBe(false)
})

test("fsScopeDecision omits save when absent or empty", async () => {
  const absent = stubPermission()
  await fsScopeDecision(absent.permission, v2Context(), ["/repo/src/a.rs"], undefined, pureFrame)
  expect("save" in (absent.calls[0] as Record<string, unknown>)).toBe(false)

  const empty = stubPermission()
  await fsScopeDecision(empty.permission, v2Context(), ["/repo/src/a.rs"], [], pureFrame)
  expect("save" in (empty.calls[0] as Record<string, unknown>)).toBe(false)
})

// MULTI-RESOURCE (mv/cp both-paths) — BOTH paths in ONE assert call, so a denied
// source OR a denied destination refuses the whole operation. The rule sets make
// the anchoring observable: the decision asserts `src/a.rs` / `dst/b.rs`, which
// the `src/**` / `dst/**` allows match (an absolute spelling would not).

test("fsScopeDecision asserts both mv/cp paths in one anchored call and allows when neither is denied", async () => {
  const { permission, calls } = scopeRulePermission([
    { action: "*", resource: "*", effect: "allow" },
    { action: "edit", resource: "*", effect: "deny" },
    { action: "edit", resource: "src/**", effect: "allow" },
    { action: "edit", resource: "dst/**", effect: "allow" },
  ])
  const d = await fsScopeDecision(
    permission,
    v2Context(),
    ["/repo/src/a.rs", "/repo/dst/b.rs"],
    undefined,
    pureFrame,
  )
  expect(d.allowed).toBe(true)
  expect(d.reason).toBe(null)
  expect(calls).toHaveLength(1)
  expect((calls[0] as Record<string, unknown>).resources).toEqual(["src/a.rs", "dst/b.rs"])
})

test("fsScopeDecision refuses when a mv/cp SOURCE is denied", async () => {
  const { permission, calls } = scopeRulePermission([
    { action: "*", resource: "*", effect: "allow" },
    { action: "edit", resource: "*", effect: "deny" },
    { action: "edit", resource: "dst/**", effect: "allow" },
  ])
  const d = await fsScopeDecision(
    permission,
    v2Context(),
    ["/repo/src/a.rs", "/repo/dst/b.rs"],
    undefined,
    pureFrame,
  )
  expect(d.allowed).toBe(false)
  expect(d.reason).toBe("path denied: src/a.rs")
  // Both paths still reached the single call — the deny is opencode's across the set.
  expect((calls[0] as Record<string, unknown>).resources).toEqual(["src/a.rs", "dst/b.rs"])
})

test("fsScopeDecision refuses when a mv/cp DESTINATION is denied", async () => {
  const { permission, calls } = scopeRulePermission([
    { action: "*", resource: "*", effect: "allow" },
    { action: "edit", resource: "*", effect: "deny" },
    { action: "edit", resource: "src/**", effect: "allow" },
  ])
  const d = await fsScopeDecision(
    permission,
    v2Context(),
    ["/repo/src/a.rs", "/repo/dst/b.rs"],
    undefined,
    pureFrame,
  )
  expect(d.allowed).toBe(false)
  expect(d.reason).toBe("path denied: dst/b.rs")
  expect((calls[0] as Record<string, unknown>).resources).toEqual(["src/a.rs", "dst/b.rs"])
})

// MAIN-ANCHORING THROUGH THE DECISION — only the main-anchored resource reaches
// `permission.assert`; the rule set (not the shape) makes the direction
// observable, since an absolute spelling matches the broad `*` edit deny.

test("fsScopeDecision asserts only the main-anchored resource, so the worktree allow matches", async () => {
  const { permission, calls } = scopeRulePermission([
    { action: "*", resource: "*", effect: "allow" },
    { action: "edit", resource: "*", effect: "deny" },
    { action: "edit", resource: "apg/.worktrees/*/**", effect: "allow" },
  ])
  const d = await fsScopeDecision(
    permission,
    v2Context(),
    ["/repo/apg/.worktrees/fs-scope-frame/x.txt"],
    undefined,
    pureFrame,
  )
  expect(d.allowed).toBe(true)
  expect(calls).toHaveLength(1)
  // Had the decision asserted the absolute form, the `*` edit deny would fire.
  expect((calls[0] as Record<string, unknown>).resources).toEqual([
    "apg/.worktrees/fs-scope-frame/x.txt",
  ])
})

test("fsScopeDecision keeps an external path absolute and refuses it", async () => {
  const { permission, calls } = scopeRulePermission([
    { action: "*", resource: "*", effect: "allow" },
    { action: "edit", resource: "*", effect: "deny" },
    { action: "edit", resource: "apg/.worktrees/*/**", effect: "allow" },
  ])
  const d = await fsScopeDecision(permission, v2Context(), ["/outside/x.rs"], undefined, pureFrame)
  expect(d.allowed).toBe(false)
  expect(d.reason).toContain("path denied: /outside/x.rs")
  // No main-relative spelling exists outside `/repo`, so it is asserted unchanged.
  expect((calls[0] as Record<string, unknown>).resources).toEqual(["/outside/x.rs"])
})

// SAVE — a populated always-allow set rides the SAME single call as `resources`,
// main-anchored, because what it stores is what the next decision matches.

test("fsScopeDecision maps a populated save alongside resources in ONE call", async () => {
  const { permission, calls } = scopeRulePermission([
    { action: "*", resource: "*", effect: "allow" },
    { action: "edit", resource: "*", effect: "deny" },
    { action: "edit", resource: "src/**", effect: "allow" },
  ])
  const d = await fsScopeDecision(
    permission,
    v2Context(),
    ["/repo/src/a.rs"],
    ["/repo/src/**"],
    pureFrame,
  )
  expect(d.allowed).toBe(true)
  expect(calls).toHaveLength(1)
  const args = calls[0] as Record<string, unknown>
  expect(args.resources).toEqual(["src/a.rs"])
  // The pattern rides the SAME single call through the frame seam: its literal
  // prefix goes through the (here identity) canonicaliser and the glob tail is
  // preserved verbatim, then both are mapped into the main-anchored frame.
  expect(args.save).toEqual(["src/**"])
})

// SAVE-FRAME MAPPING — patterns ride the same canonical frame as resources.
//
// `fsScopeDecision` splits each always-allow pattern at its FIRST wildcard: a
// wildcard-free pattern is canonicalised wholesale; an ABSOLUTE literal prefix
// is canonicalised (the real `canonicalPath` resolves its deepest existing
// ancestor) with the glob tail re-appended verbatim; an EMPTY or RELATIVE prefix
// is NOT canonicalised at all (canonicalPath("") would resolve `process.cwd()`,
// and a relative prefix is cwd-coupled) and the whole pattern maps lexically.
// These pins drive that split through the injected pure frame — a
// symlink-mapping canonicaliser stand-in (`/var` is a symlink to `/private/var`
// on macOS) plus a fixed root — so no real fs/git/process I/O runs.

const CANON_ROOT = "/private/var/repo"
const canonicaliseSymlink = (p: string) => p.replace(/^\/var\//, "/private/var/")
const symlinkFrame = {
  resolveRoot: async () => CANON_ROOT,
  canonicalise: canonicaliseSymlink,
}

test("fsScopeDecision anchors a RAW save pattern like its canonical spelling", async () => {
  // The rules make the anchoring observable: the anchored `src/a.rs` matches the
  // `src/**` allow, an absolute spelling would hit the broad `*` edit deny.
  const { permission, calls } = scopeRulePermission([
    { action: "*", resource: "*", effect: "allow" },
    { action: "edit", resource: "*", effect: "deny" },
    { action: "edit", resource: "src/**", effect: "allow" },
  ])
  const d = await fsScopeDecision(
    permission,
    v2Context(),
    ["/private/var/repo/src/a.rs"],
    ["/var/repo/src/a.rs"],
    symlinkFrame,
  )
  expect(d.allowed).toBe(true)
  expect(d.reason).toBe(null)
  expect(calls).toHaveLength(1)
  const args = calls[0] as Record<string, unknown>
  expect(args.resources).toEqual(["src/a.rs"])
  // A raw `path.resolve`d save (what the fs tools pass) must anchor identically
  // to its canonical spelling, or an "always allow" stored today never matches
  // the next decision's resource.
  expect(args.save).toEqual(["src/a.rs"])
})

test("fsScopeDecision keeps a save glob tail verbatim while its literal prefix anchors", async () => {
  const { permission, calls } = stubPermission()
  const d = await fsScopeDecision(
    permission,
    v2Context(),
    ["/private/var/repo/src/a.rs"],
    ["/var/repo/src/**"],
    symlinkFrame,
  )
  expect(d.allowed).toBe(true)
  expect(calls).toHaveLength(1)
  const args = calls[0] as Record<string, unknown>
  expect(args.resources).toEqual(["src/a.rs"])
  // The literal prefix canonicalises AND anchors, the tail rides verbatim — not
  // `src**` (the separator survives), not the absolute pattern.
  expect(args.save).toEqual(["src/**"])
})

test("fsScopeDecision canonicalises an absolute save prefix whose directory does not exist", async () => {
  const seen: string[] = []
  const recordingFrame = {
    resolveRoot: async () => CANON_ROOT,
    canonicalise: (p: string) => {
      seen.push(p)
      return canonicaliseSymlink(p)
    },
  }
  const { permission, calls } = stubPermission()
  const d = await fsScopeDecision(
    permission,
    v2Context(),
    ["/private/var/repo/src/a.rs"],
    ["/var/repo/denied/**"],
    recordingFrame,
  )
  expect(d.allowed).toBe(true)
  const args = calls[0] as Record<string, unknown>
  expect(args.resources).toEqual(["src/a.rs"])
  // `denied/` need not exist: an ABSOLUTE literal prefix is still canonicalised
  // (deepest-existing-ancestor rule), so it anchors instead of escaping absolute.
  expect(args.save).toEqual(["denied/**"])
  expect(seen).toContain("/var/repo/denied/")
})

test("fsScopeDecision maps empty-prefix and relative save patterns lexically and never canonicalises them", async () => {
  const seen: string[] = []
  const patterns = ["*", "**/x", "src/**"]
  const guardedFrame = {
    resolveRoot: async () => CANON_ROOT,
    canonicalise: (p: string) => {
      seen.push(p)
      // A save pattern reaching the canonicaliser is exactly the bug these pins
      // exist to catch: `canonicalPath("")` resolves `process.cwd()` and a
      // relative prefix is cwd-coupled, so neither may be canonicalised.
      if (patterns.includes(p)) throw new Error(`save pattern canonicalised: ${p}`)
      return canonicaliseSymlink(p)
    },
  }
  const { permission, calls } = stubPermission()
  const d = await fsScopeDecision(
    permission,
    v2Context(),
    ["/private/var/repo/src/a.rs"],
    patterns,
    guardedFrame,
  )
  expect(d.allowed).toBe(true)
  expect(calls).toHaveLength(1)
  const args = calls[0] as Record<string, unknown>
  // The resource still rides the canonicaliser — the resource pin is not weakened.
  expect(args.resources).toEqual(["src/a.rs"])
  expect(args.save).toEqual(["*", "**/x", "src/**"])
  // ...and no save pattern reached it (the throw above is the first proof).
  for (const p of patterns) expect(seen).not.toContain(p)
})

// UNRESOLVABLE FRAME ROOT — fail closed BEFORE any assert: nothing escapes, in
// any spelling, with or without a `save` set.

test("fsScopeDecision refuses when the frame root cannot be resolved and never asserts", async () => {
  const nullFrame = {
    resolveRoot: async () => null,
    canonicalise: (p: string) => p,
  }
  const { permission, calls } = stubPermission()
  const d = await fsScopeDecision(
    permission,
    v2Context(),
    ["/repo/src/a.rs"],
    undefined,
    nullFrame,
  )
  expect(d.allowed).toBe(false)
  expect(d.reason).toContain("main checkout root unavailable")
  expect(calls).toHaveLength(0)

  // A populated `save` does not escape either.
  const withSave = stubPermission()
  const ds = await fsScopeDecision(
    withSave.permission,
    v2Context(),
    ["/repo/src/a.rs"],
    ["/repo/src/**"],
    nullFrame,
  )
  expect(ds.allowed).toBe(false)
  expect(ds.reason).toContain("main checkout root unavailable")
  expect(withSave.calls).toHaveLength(0)
})

// MAIN-ANCHORED RESOURCE — the pure path→resource mapping.

test("mainAnchoredResource maps a path inside the frame root to a bare `/`-separated resource", () => {
  // Both arguments are taken as already canonical: the frame root is a stand-in
  // string and the mapping does no fs/git/process work.
  const root = "/repo"
  // Strictly below the root: the bare spelling an agent's `edit` globs are
  // written against, with no leading `/`.
  expect(mainAnchoredResource("/repo/src/a.rs", root)).toBe("src/a.rs")
  // A nested worktree path keeps its full depth.
  expect(mainAnchoredResource("/repo/apg/.worktrees/fs-scope-frame/x.txt", root)).toBe(
    "apg/.worktrees/fs-scope-frame/x.txt",
  )
})

test("mainAnchoredResource leaves a root-equal, outside, or escaping path unchanged absolute", () => {
  const root = "/repo"
  // Root-equal has no non-empty relative spelling to offer.
  expect(mainAnchoredResource("/repo", root)).toBe("/repo")
  // Genuinely outside the frame root.
  expect(mainAnchoredResource("/other/x.rs", root)).toBe("/other/x.rs")
  // Escaping the root via `..`.
  expect(mainAnchoredResource("/repo/../x.rs", root)).toBe("/repo/../x.rs")
  // A sibling whose STRING starts with the root is not inside it: the relative
  // form is `../repo-other/x.rs`, so it must stay absolute.
  expect(mainAnchoredResource("/repo-other/x.rs", root)).toBe("/repo-other/x.rs")
})

test("mainAnchoredResource takes no canonicaliser argument", () => {
  // Canonicalisation is fsScopeDecision's injected seam, never this mapping's:
  // the helper is exactly `(path, root)` and stays pure.
  expect(mainAnchoredResource.length).toBe(2)
})
