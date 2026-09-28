// Unit tests for the shared suite plumbing (`opencode-suite/lib/apg.ts`).
//
// Runner: `bun test` (opencode-suite is a Bun package — `apg.ts` uses `Bun.$`
// in `runCypher`/`runCli`). Only the side-effect-free helpers are exercised
// here; the subprocess/fs paths are covered by the opt-in boundary e2e.
import { test, expect } from "bun:test"

import {
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
  editAllowGlobs,
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

// agent-fs-tools phase-01.task-7: the PURE self-enforcement core for the fs
// tools. `fsScopeDecision` is side-effect-free (no fs, no process, no db), so
// it lives in the `bun test` unit tier; the real-fs application path is the
// opt-in boundary e2e. Two INDEPENDENT tests, both required to allow.
test("fsScopeDecision allows a path matched by a granted glob inside the boundary", () => {
  const d = fsScopeDecision("/repo", "/repo/src/a.rs", ["src/*.rs"])
  expect(d.allowed).toBe(true)
  expect(d.globMatched).toBe(true)
  expect(d.inBoundary).toBe(true)
  expect(d.relativePath).toBe("src/a.rs")
  expect(d.reason).toBe(null)
})

test("fsScopeDecision refuses a path outside the granted globs", () => {
  const d = fsScopeDecision("/repo", "/repo/build/x.rs", ["src/*.rs"])
  expect(d.allowed).toBe(false)
  expect(d.globMatched).toBe(false)
  expect(d.inBoundary).toBe(true)
  expect(d.reason).toContain("granted globs")
})

test("fsScopeDecision refuses a ..-escaping path even when a glob would match", () => {
  const d = fsScopeDecision("/repo", "/repo/../etc/passwd", ["**/*"])
  expect(d.allowed).toBe(false)
  expect(d.globMatched).toBe(false)
  expect(d.inBoundary).toBe(false)
  expect(d.reason).toContain("boundary")
})

test("fsScopeDecision resolves a relative candidate against projectRoot", () => {
  const d = fsScopeDecision("/repo", "src/deep/b.rs", ["**/*.rs"])
  expect(d.allowed).toBe(true)
  expect(d.relativePath).toBe("src/deep/b.rs")
})

test("fsScopeDecision: `*` does not cross /, `**` does, `**/` matches zero dirs", () => {
  expect(fsScopeDecision("/repo", "/repo/src/deep/a.rs", ["src/*.rs"]).allowed).toBe(false)
  expect(fsScopeDecision("/repo", "/repo/src/deep/a.rs", ["src/**/*.rs"]).allowed).toBe(true)
  expect(fsScopeDecision("/repo", "/repo/a.rs", ["**/*.rs"]).allowed).toBe(true)
})

// worktree-write-scope phase-01.task-7: the PURE main-anchored decision. When
// the caller supplies `project` (a worktree name, or `null` from the main
// checkout) `fsScopeDecision` resolves grants and candidate against the MAIN
// root and structurally refuses any candidate that lands in the main checkout —
// a root glob may match and `allowed` still stays false. Still side-effect-free
// (no fs/git/process), so it lives in the `bun test` unit tier.
test("fsScopeDecision (main-anchored) refuses a main-checkout path even when a root glob matches", () => {
  const d = fsScopeDecision(
    "/main",
    "/main/opencode-suite/lib/apg.test.ts",
    ["opencode-suite/lib/apg.test.ts"],
    "proj",
  )
  expect(d.allowed).toBe(false)
  expect(d.inBoundary).toBe(true)
  // The root glob DOES match — the refusal is the worktree rule, not the glob.
  expect(d.globMatched).toBe(true)
  expect(d.reason).toBe("path resolves into the main checkout")
  // A `null` project (the caller is in the main checkout) refuses the same way.
  const fromMain = fsScopeDecision(
    "/main",
    "/main/src/a.rs",
    ["src/*.rs"],
    null,
  )
  expect(fromMain.globMatched).toBe(true)
  expect(fromMain.allowed).toBe(false)
  expect(fromMain.reason).toBe("path resolves into the main checkout")
})

test("fsScopeDecision (main-anchored) allows a worktree path under an owned apg/.worktrees/*/<glob> grant", () => {
  const d = fsScopeDecision(
    "/main",
    "/main/apg/.worktrees/proj/opencode-suite/lib/apg.test.ts",
    ["apg/.worktrees/*/opencode-suite/**/*.ts"],
    "proj",
  )
  expect(d.allowed).toBe(true)
  expect(d.inBoundary).toBe(true)
  expect(d.globMatched).toBe(true)
  expect(d.reason).toBe(null)
  expect(d.relativePath).toBe(
    "apg/.worktrees/proj/opencode-suite/lib/apg.test.ts",
  )
  // A `null` project accepts ANY project worktree that matches a granted glob.
  const anyWorktree = fsScopeDecision(
    "/main",
    "/main/apg/.worktrees/proj/opencode-suite/lib/apg.test.ts",
    ["apg/.worktrees/*/opencode-suite/**/*.ts"],
    null,
  )
  expect(anyWorktree.allowed).toBe(true)
  expect(anyWorktree.reason).toBe(null)
})

test("fsScopeDecision (main-anchored) refuses an unowned worktree path", () => {
  // A sibling project's worktree is not under THIS project's worktree.
  const sibling = fsScopeDecision(
    "/main",
    "/main/apg/.worktrees/other/opencode-suite/lib/apg.test.ts",
    ["apg/.worktrees/proj/opencode-suite/**"],
    "proj",
  )
  expect(sibling.allowed).toBe(false)
  expect(sibling.inBoundary).toBe(true)
  expect(sibling.reason).toBe("path resolves into the main checkout")
  // Inside the owned worktree but outside the granted globs: refused for the
  // glob, not the worktree rule.
  const unownedGlob = fsScopeDecision(
    "/main",
    "/main/apg/.worktrees/proj/src/a.rs",
    ["apg/.worktrees/proj/opencode-suite/**"],
    "proj",
  )
  expect(unownedGlob.allowed).toBe(false)
  expect(unownedGlob.globMatched).toBe(false)
  expect(unownedGlob.reason).toBe(
    "path is not within the acting agent's granted globs",
  )
})

test("fsScopeDecision (main-anchored) refuses a .. or absolute escape even when a glob would match", () => {
  const absolute = fsScopeDecision("/main", "/etc/passwd", ["**/*"], "proj")
  expect(absolute.allowed).toBe(false)
  expect(absolute.inBoundary).toBe(false)
  expect(absolute.reason).toBe("path escapes the project/worktree boundary")
  const dotdot = fsScopeDecision(
    "/main",
    "/main/apg/.worktrees/proj/../../../../etc/passwd",
    ["apg/.worktrees/*/**"],
    "proj",
  )
  expect(dotdot.allowed).toBe(false)
  expect(dotdot.inBoundary).toBe(false)
  expect(dotdot.globMatched).toBe(false)
  expect(dotdot.reason).toBe("path escapes the project/worktree boundary")
})

// worktree-write-scope phase-04.task-10: the PURE last-matching-rule semantics
// of `fsScopeDecision` over ORDERED `{ glob, allow }` rules (BOTH allow and
// deny, in FILE ORDER). Still side-effect-free (no fs/git/process/db), so these
// stay in the `bun test` unit tier. A bare string entry is the legacy ALLOW
// rule, so the older cases above keep working.
test("fsScopeDecision: a later deny overrides an earlier broad allow", () => {
  // `**/*` grants everything; the later `secret/**` deny revokes that subtree.
  // A path matching BOTH rules is refused because the LAST matching rule is the
  // deny — the broad allow must not win just by appearing.
  const rules = [
    { glob: "**/*", allow: true },
    { glob: "secret/**", allow: false },
  ]
  const denied = fsScopeDecision("/repo", "/repo/secret/x.rs", rules)
  expect(denied.allowed).toBe(false)
  expect(denied.globMatched).toBe(true)
  expect(denied.reason).toBe("path is not within the acting agent's granted globs")
  // A path the deny does not match stays granted by the earlier allow.
  const granted = fsScopeDecision("/repo", "/repo/src/a.rs", rules)
  expect(granted.allowed).toBe(true)
  expect(granted.globMatched).toBe(true)
  expect(granted.reason).toBe(null)
})

test("fsScopeDecision: a deny followed by a later allow is allowed (last match wins)", () => {
  // The bug this pins: a naive "deny-anywhere-wins" implementation refuses
  // `src/a.rs` because SOME deny rule matched. The deny is EARLIER than the
  // allow, so the LAST matching rule (the allow) must win.
  const rules = [
    { glob: "**/*", allow: false },
    { glob: "src/**/*.rs", allow: true },
  ]
  const d = fsScopeDecision("/repo", "/repo/src/a.rs", rules)
  expect(d.allowed).toBe(true)
  expect(d.globMatched).toBe(true)
  expect(d.inBoundary).toBe(true)
  expect(d.reason).toBe(null)
})

test("fsScopeDecision: a path matched by no rule is refused (deny-by-default)", () => {
  // An empty scope refuses everything.
  const empty = fsScopeDecision("/repo", "/repo/src/a.rs", [])
  expect(empty.allowed).toBe(false)
  expect(empty.globMatched).toBe(false)
  expect(empty.reason).toBe("path is not within the acting agent's granted globs")
  // A non-empty scope still refuses a path no rule mentions.
  const rules = [
    { glob: "**/*.rs", allow: true },
    { glob: "build/**", allow: false },
  ]
  const unmatched = fsScopeDecision("/repo", "/repo/docs/readme.md", rules)
  expect(unmatched.allowed).toBe(false)
  expect(unmatched.globMatched).toBe(false)
  expect(unmatched.reason).toBe("path is not within the acting agent's granted globs")
})

// The parser half: `editAllowGlobs` must keep BOTH allow and deny entries in
// FILE ORDER — the ordered `{ glob, allow }` list's order and membership, not an
// allow-only projection and not a reordered (allow-then-deny) group.
test("editAllowGlobs keeps both allow and deny entries in file order", () => {
  const body = [
    "permission:",
    "  edit:",
    '    "src/**": allow',
    '    "src/secret/**": deny',
    '    "src/public/**": allow',
  ].join("\n")
  expect(editAllowGlobs(body)).toEqual([
    { glob: "src/**", allow: true },
    { glob: "src/secret/**", allow: false },
    { glob: "src/public/**", allow: true },
  ])
})

// A deny that appears FIRST must not be hoisted or dropped: order is precedence,
// so the parser preserves it verbatim (and duplicates are kept, not deduped).
test("editAllowGlobs preserves a leading deny and duplicate entries verbatim", () => {
  const body = [
    "permission:",
    "  edit:",
    '    "**/*": deny',
    '    "src/**": allow',
    '    "src/**": allow',
  ].join("\n")
  expect(editAllowGlobs(body)).toEqual([
    { glob: "**/*", allow: false },
    { glob: "src/**", allow: true },
    { glob: "src/**", allow: true },
  ])
})


