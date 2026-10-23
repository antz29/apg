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
// outcome to `{ allowed, reason }`. It is side-effect-free — no fs, no process,
// no db, and the `assert` is a pure in-memory stub — so it lives in the `bun
// test` unit tier; the real-fs application path and the `findApgRoot` /
// `mainCheckoutRoot` root resolution are the opt-in boundary e2e.
//
// `permission.assert` is never imported from `@opencode/plugin` here: it is a
// structurally-typed `{ assert }` seam the tools pass through, so a stub models
// opencode's decision (resolve = allow, reject = deny) with no live opencode 2.x.

/** A faked opencode 2.x tool-executor context — the V2 shape
 *  `{ agent, sessionID, messageID, id }` (no V1 `directory`/`worktree`). */
function v2Context(overrides: Partial<ToolContext> = {}): ToolContext {
  return { agent: "implementer", sessionID: "s1", messageID: "m1", id: "c1", ...overrides }
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

/** An opencode-like stub that evaluates the WHOLE resource set of a single call
 *  and denies when ANY resource is in `deniedPaths` — the any-deny precedence
 *  opencode applies across a call's resources. Used to model a denied source vs
 *  a denied destination of one `apg_mv`/`apg_cp`. */
function scopeDenyingPaths(deniedPaths: readonly string[]): {
  permission: { assert: (args: unknown) => Promise<void> }
  calls: unknown[]
} {
  const calls: unknown[] = []
  const permission = {
    assert: async (args: unknown): Promise<void> => {
      calls.push(args)
      const { resources } = args as { resources: readonly string[] }
      const hit = resources.find((r) => deniedPaths.includes(r))
      if (hit !== undefined) throw new Error(`path denied: ${hit}`)
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

// ALLOW / DENY — the assert outcome reduced to the decision.

test("fsScopeDecision allows when permission.assert resolves", async () => {
  const { permission } = stubPermission()
  const d = await fsScopeDecision(permission, v2Context(), ["/repo/src/a.rs"])
  expect(d.allowed).toBe(true)
  expect(d.reason).toBe(null)
})

test("fsScopeDecision refuses with the thrown error's message", async () => {
  const { permission } = stubPermission(new Error("path is outside the granted scope"))
  const d = await fsScopeDecision(permission, v2Context(), ["/repo/src/a.rs"])
  expect(d.allowed).toBe(false)
  expect(d.reason).toBe("path is outside the granted scope")
})

test("fsScopeDecision extracts reason from a rejection carrying only a reason field", async () => {
  // A rejected permission prompt surfaces as a Tool.Error-shaped object whose
  // refusal lives on `reason`, not `message`.
  const { permission } = stubPermission({ reason: "denied by the user prompt" })
  const d = await fsScopeDecision(permission, v2Context(), ["/repo/src/a.rs"])
  expect(d.allowed).toBe(false)
  expect(d.reason).toBe("denied by the user prompt")
})

test("fsScopeDecision refuses a rejected promise (async deny)", async () => {
  const permission = {
    assert: (_args: unknown): Promise<void> => Promise.reject(new Error("async deny")),
  }
  const d = await fsScopeDecision(permission, v2Context(), ["/repo/src/a.rs"])
  expect(d.allowed).toBe(false)
  expect(d.reason).toBe("async deny")
})

// ASSERT-ARGS CONSTRUCTION — the call built from the V2 context.

test("fsScopeDecision builds the assert args from the V2 context", async () => {
  const ctx = v2Context({ agent: "implementer", sessionID: "s1", messageID: "m1", id: "c1" })
  const { permission, calls } = stubPermission()
  const resources = ["/repo/src/a.rs", "/repo/src/b.rs"]
  await fsScopeDecision(permission, ctx, resources)

  expect(calls).toHaveLength(1)
  const args = calls[0] as Record<string, unknown>
  expect(args.action).toBe("edit")
  // Passed through unchanged (same array identity), not copied/rebased.
  expect(args.resources).toBe(resources)
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
  await fsScopeDecision(permission, v2Context({ agent: "" }), ["/repo/src/a.rs"])
  const args = calls[0] as Record<string, unknown>
  expect("agent" in args).toBe(false)
})

test("fsScopeDecision omits save when absent or empty and passes a non-empty save through", async () => {
  const absent = stubPermission()
  await fsScopeDecision(absent.permission, v2Context(), ["/repo/src/a.rs"])
  expect("save" in (absent.calls[0] as Record<string, unknown>)).toBe(false)

  const empty = stubPermission()
  await fsScopeDecision(empty.permission, v2Context(), ["/repo/src/a.rs"], [])
  expect("save" in (empty.calls[0] as Record<string, unknown>)).toBe(false)

  const populated = stubPermission()
  const save = ["/repo/src/**"]
  await fsScopeDecision(populated.permission, v2Context(), ["/repo/src/a.rs"], save)
  expect((populated.calls[0] as Record<string, unknown>).save).toBe(save)
})

// MULTI-RESOURCE (mv/cp both-paths) — BOTH paths in ONE assert call, so a denied
// source OR a denied destination refuses the whole operation.

test("fsScopeDecision asserts both mv/cp paths in one call and allows when neither is denied", async () => {
  const { permission, calls } = scopeDenyingPaths([])
  const d = await fsScopeDecision(permission, v2Context(), ["/repo/src/a.rs", "/repo/dst/b.rs"])
  expect(d.allowed).toBe(true)
  expect(d.reason).toBe(null)
  expect(calls).toHaveLength(1)
  expect((calls[0] as Record<string, unknown>).resources).toEqual([
    "/repo/src/a.rs",
    "/repo/dst/b.rs",
  ])
})

test("fsScopeDecision refuses when a mv/cp SOURCE is denied", async () => {
  const { permission, calls } = scopeDenyingPaths(["/repo/src/a.rs"])
  const d = await fsScopeDecision(permission, v2Context(), ["/repo/src/a.rs", "/repo/dst/b.rs"])
  expect(d.allowed).toBe(false)
  expect(d.reason).toBe("path denied: /repo/src/a.rs")
  // Both paths still reached the single call — the deny is opencode's across the set.
  expect((calls[0] as Record<string, unknown>).resources).toEqual([
    "/repo/src/a.rs",
    "/repo/dst/b.rs",
  ])
})

test("fsScopeDecision refuses when a mv/cp DESTINATION is denied", async () => {
  const { permission, calls } = scopeDenyingPaths(["/repo/dst/b.rs"])
  const d = await fsScopeDecision(permission, v2Context(), ["/repo/src/a.rs", "/repo/dst/b.rs"])
  expect(d.allowed).toBe(false)
  expect(d.reason).toBe("path denied: /repo/dst/b.rs")
  expect((calls[0] as Record<string, unknown>).resources).toEqual([
    "/repo/src/a.rs",
    "/repo/dst/b.rs",
  ])
})
