// Tests for the TypeScript scanner frontend (`src/tslib/scanner.ts`).
//
// Runner: `node --test` — the scanner runs under node. Node 22.7+ executes the
// `.ts` source directly (type stripping), so the harness drives the real
// scanner over a scratch fixture and asserts the unified-JSONL facts. Pure
// package-identity logic gets its own `node --test` unit module alongside this
// file as that logic is extracted.
import { test } from "node:test"
import assert from "node:assert/strict"
import { execFileSync } from "node:child_process"
import fs from "node:fs"
import os from "node:os"
import path from "node:path"

const scanner = path.join(import.meta.dirname, "scanner.ts")

/** Runs the real scanner over `dir` (plus extra argv) and parses the emitted
 *  unified-JSONL records (the scanner's progress line is on stderr). */
function run(dir: string, extraArgs: string[] = []): any[] {
  const out = execFileSync(process.execPath, [scanner, dir, ...extraArgs], { encoding: "utf8" })
  return out
    .split("\n")
    .filter((l) => l.startsWith("{"))
    .map((l) => JSON.parse(l))
}

/** Writes a scratch tree, hands its directory to `fn`, then removes it. */
function fixture<T>(files: Record<string, string>, fn: (dir: string) => T): T {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "apg-ts-test-"))
  try {
    for (const [rel, body] of Object.entries(files)) {
      const p = path.join(dir, rel)
      fs.mkdirSync(path.dirname(p), { recursive: true })
      fs.writeFileSync(p, body)
    }
    return fn(dir)
  } finally {
    fs.rmSync(dir, { recursive: true, force: true })
  }
}

/** Writes a scratch package and runs the real scanner over it. */
function scan(files: Record<string, string>, extraArgs: string[] = []): any[] {
  return fixture(files, (dir) => run(dir, extraArgs))
}

test("emits a module plus file/struct/function records for a single package", () => {
  const records = scan({
    "package.json": JSON.stringify({ name: "fixture", private: true }),
    "src.ts":
      "export class Point { constructor(public x: number) {} }\n" +
      "export function add(a: number, b: number): number { return a + b; }\n",
  })

  const modules = records.filter((r) => r.type === "module")
  assert.equal(modules.length, 1, "one package => one module")
  assert.equal(modules[0].fqn, "fixture")

  const files = records.filter((r) => r.type === "file")
  assert.equal(files.length, 1, "one source file => one file record")
  assert.equal(files[0].parent, "fixture")

  const structs = records.filter((r) => r.type === "struct")
  const point = structs.find((r) => r.name === "Point")
  assert.ok(point, "Point struct emitted")
  assert.equal(point.parent, "fixture.src")

  const funcs = records.filter((r) => r.type === "function")
  assert.ok(
    funcs.some((r) => r.name === "add"),
    "add function emitted",
  )
})

test("each ES-module file is its own namespace (same class name in two files)", () => {
  const records = scan({
    "package.json": JSON.stringify({ name: "fixture", private: true }),
    "a.ts": "export class Button {}\n",
    "b.ts": "export class Button {}\n",
  })
  const parents = records
    .filter((r) => r.type === "struct" && r.name === "Button")
    .map((r) => r.parent)
  assert.equal(parents.length, 2, "both Button declarations emitted")
  assert.notEqual(parents[0], parents[1], "the two files are distinct namespaces")
})

// A two-package npm workspace: the module selection cases select package `a`.
const workspace = {
  "package.json": JSON.stringify({ name: "root", private: true, workspaces: ["packages/*"] }),
  "packages/a/package.json": JSON.stringify({ name: "a", private: true }),
  "packages/a/a.ts": "export const a = 1\n",
  "packages/b/package.json": JSON.stringify({ name: "b", private: true }),
  "packages/b/b.ts": "export const b = 2\n",
}

const filePaths = (records: any[]) =>
  records.filter((r) => r.type === "file").map((r) => r.path).sort()

test("--module selects the named workspace package in relative and absolute form", () => {
  fixture(workspace, (dir) => {
    const relative = filePaths(run(dir, ["--module", "packages/a"]))
    assert.equal(relative.length, 1, "only package a's file record is emitted")
    assert.ok(relative[0].endsWith(path.join("packages", "a", "a.ts")))

    const absolute = filePaths(run(dir, ["--module", path.join(dir, "packages", "a")]))
    assert.deepEqual(absolute, relative, "the absolute form admits the identical set")
  })
})

test("--module spelled through a symlinked package dir selects the same package", (t) => {
  fixture(workspace, (dir) => {
    try {
      fs.symlinkSync(path.join(dir, "packages", "a"), path.join(dir, "alias"), "dir")
    } catch {
      t.skip("symlink creation denied on this platform")
      return
    }
    const linked = filePaths(run(dir, ["--module", "alias"]))
    assert.deepEqual(linked, filePaths(run(dir, ["--module", "packages/a"])))
  })
})
