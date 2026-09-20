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

