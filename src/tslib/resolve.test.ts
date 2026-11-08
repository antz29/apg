// Unit tests for `resolve::calleeIdentity` (`src/tslib/resolve.mjs`).
//
// Runner: `node --test`, matching `identity.test.ts`. Pure and in-memory:
// `ts.createSourceFile` over string literals — no filesystem, process, git or
// database. `config.root` is a synthetic absolute root (never a real checkout)
// so a nameless callee's repo-relative sentinel is observable, and the test can
// assert the root itself never leaks into it.
import { test } from "node:test"
import assert from "node:assert/strict"
import path from "node:path"
import ts from "typescript"
import { calleeIdentity } from "./resolve.mjs"
import { config } from "./state.mjs"

config.root = "/apg-repo"

/** Parses `code` and returns the identity of its first CallExpression's callee.
 *  The synthetic file name sits under `config.root`, so `calleeIdentity`'s
 *  `path.relative` renders a repo-relative site sentinel. */
export function calleeOf(code: string, file = path.join(config.root, "site.ts")): string {
  const sf = ts.createSourceFile(file, code, ts.ScriptTarget.Latest, true)
  let call: ts.CallExpression | undefined
  const visit = (node: ts.Node): void => {
    if (call) return
    if (ts.isCallExpression(node)) {
      call = node
      return
    }
    ts.forEachChild(node, visit)
  }
  visit(sf)
  if (!call) throw new Error(`no call expression in: ${code}`)
  return calleeIdentity(call.expression, sf)
}

test("a bare identifier callee is preserved byte-for-byte", () => {
  const callee = "computeContentHash"
  assert.equal(calleeOf(`${callee}()`), callee)
})

test("a callee name longer than 64 characters comes back whole", () => {
  const callee = "a".repeat(70)
  const identity = calleeOf(`${callee}()`)
  assert.equal(identity, callee)
  assert.equal(identity.length, 70)
  assert.ok(identity.length > 64, "not truncated to 64")
  assert.ok(!identity.includes("…"), "no ellipsis truncation")
})

test("a nameless compound callee gets a stable, site-distinct sentinel with no source text or checkout path", () => {
  const source = "() => 1"
  const first = calleeOf(`(${source})()`)
  const again = calleeOf(`(${source})()`)
  assert.equal(first, again, "stable for one site")

  // Same synthetic file name, callee at a different position: a different site.
  const elsewhere = calleeOf(`\n\n  (${source})()`)
  assert.notEqual(first, elsewhere, "distinct across two sites")

  for (const sentinel of [first, elsewhere]) {
    assert.ok(!sentinel.includes(source), `never the callee's source text: ${sentinel}`)
    assert.ok(!sentinel.includes("=>"), `no source fragment: ${sentinel}`)
    assert.ok(!sentinel.includes(config.root), `never the checkout path: ${sentinel}`)
    assert.ok(!sentinel.includes("/apg-repo"), `never the synthetic root: ${sentinel}`)
  }
})
