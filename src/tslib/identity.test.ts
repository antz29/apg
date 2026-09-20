// Unit tests for the side-effect-free package-identity module
// (`src/tslib/identity.mjs`).
//
// Runner: `node --test` — Node executes the `.ts` test directly (type
// stripping) and imports the plain `.mjs` module. Pure and in-memory: no
// filesystem, database, git or process. The real-I/O scanner checks stay the
// Rust e2e tier (`js_only_repo_accepts_all_four_js_extensions`,
// `mixed_js_ts_repo_resolves_both_directions`).
import { test } from "node:test"
import assert from "node:assert/strict"
import path from "node:path"
import { packageIdentity } from "./identity.mjs"

test("a file inside a discovered package takes the package's declared name", () => {
  const packages = [{ dir: "/co/repo/pkg", name: "@co/ui" }]
  assert.equal(
    packageIdentity("/co/repo", packages, "/co/repo/pkg/src/components/Button.ts"),
    "@co/ui",
  )
})

test("the deepest enclosing package wins, so a file has one identity", () => {
  const packages = [
    { dir: "/co/repo", name: "root-pkg" },
    { dir: "/co/repo/pkg", name: "inner" },
  ]
  assert.equal(packageIdentity("/co/repo", packages, "/co/repo/pkg/a.ts"), "inner")
  // A sibling file outside the inner package belongs to the outer one.
  assert.equal(packageIdentity("/co/repo", packages, "/co/repo/src/a.ts"), "root-pkg")
})

test("siblings under different packages get distinct identities", () => {
  const packages = [
    { dir: "/co/repo/lib", name: "mixed-lib" },
    { dir: "/co/repo", name: "mixed" },
  ]
  assert.equal(packageIdentity("/co/repo", packages, "/co/repo/lib/index.js"), "mixed-lib")
  assert.equal(packageIdentity("/co/repo", packages, "/co/repo/src/index.js"), "mixed")
})

test("a fallback file directly in the repo base never takes the checkout basename", () => {
  const base = "/tmp/apg-js-1234-js-only/repo"
  assert.equal(path.basename(base), "repo")
  const identity = packageIdentity(base, [], path.join(base, "calc.js"))
  assert.equal(identity, "root")
  assert.notEqual(identity, path.basename(base))
  assert.ok(!identity.includes(path.basename(base)))
})

test("a fallback file in a subdirectory derives its identity from the repo base", () => {
  const base = "/co/repo"
  assert.equal(packageIdentity(base, [], "/co/repo/src/loose.js"), "src")
  assert.equal(packageIdentity(base, [], "/co/repo/a/b/loose.mjs"), "a.b")
})

test("fallback identities are checkout-independent (same layout, different base)", () => {
  const one = packageIdentity("/co/one", [], "/co/one/src/loose.js")
  const two = packageIdentity("/co/two", [], "/co/two/src/loose.js")
  assert.equal(one, two)
  assert.equal(one, "src")
})

test("no fallback identity embeds the repo base or a `..` segment", () => {
  const base = "/co/repo"
  for (const file of [
    "/co/repo/loose.js",
    "/co/repo/src/loose.ts",
    "/co/repo/target/debug/x/scanner.mjs",
  ]) {
    const identity = packageIdentity(base, [], file)
    assert.ok(!identity.startsWith("/"), `no absolute identity: ${identity}`)
    assert.ok(
      !identity.split(".").includes("..") && !identity.includes(".." + path.sep),
      `no .. segment: ${identity}`,
    )
    assert.ok(!identity.includes(base), `no checkout path: ${identity}`)
  }
})

test("the repo base itself resolves to the root identity (directory target)", () => {
  assert.equal(packageIdentity("/co/repo", [], "/co/repo"), "root")
})
