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

