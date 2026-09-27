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

