// e2e boundary test for the curated suite tools' stored<->absolute conversion.
//
// OPT-IN: real I/O (a scratch /tmp git repo scanned by the candidate `apg`
// binary, spawning `apg query`). Plain `bun test` skips it; run it with
//
//   APG_SUITE_E2E=1 APG_BINARY="$PWD/target/debug/apg" \
//     bun test tests/boundary.e2e.test.ts
//
// Never pointed at a real project — only a throwaway /tmp repo
// (`global.constraint.no-real-project-test`).
import { test, expect } from "bun:test"
import { existsSync } from "node:fs"
import fs from "node:fs"
import os from "node:os"
import path from "node:path"

import { runCypher, resolveProjectPath, csvToRows, type ToolContext } from "../lib/apg.ts"

/// The candidate binary: `APG_BINARY`, else the in-repo debug build. The suite
/// e2e is only meaningful against a binary that renders repo-relative
/// identities, so a missing candidate skips rather than asserting.
function candidateBinary(): string | null {
  const env = process.env.APG_BINARY
  if (env && existsSync(env)) return env
  const repoRoot = path.resolve(import.meta.dir, "..", "..")
  const debug = path.join(repoRoot, "target", "debug", "apg")
  return existsSync(debug) ? debug : null
}

const enabled = process.env.APG_SUITE_E2E === "1"
const binary = enabled ? candidateBinary() : null

/// A scratch git repo with one Go package, committed. Returns its root.
function scratchRepo(): string {
  const base = fs.mkdtempSync(path.join(os.tmpdir(), "apg-suite-boundary-"))
  const repo = path.join(base, "repo")
  fs.mkdirSync(path.join(repo, "pkg"), { recursive: true })
  fs.writeFileSync(
    path.join(repo, "pkg", "a.go"),
    "package pkg\n\n// A is a struct.\ntype A struct {\n\tX int\n}\n\n// Leaf returns 1.\nfunc Leaf() int { return 1 }\n",
  )
  const run = (cmd: string[]) => Bun.spawnSync({ cmd, cwd: repo, stdout: "pipe", stderr: "pipe" })
  run(["git", "init", "-q", "-b", "main"])
  run(["git", "config", "user.email", "apg@localhost"])
  run(["git", "config", "user.name", "apg"])
  run(["git", "add", "-A"])
  run(["git", "commit", "-q", "-m", "init"])
  return repo
}

test.skipIf(!enabled || !binary)(
  "e2e: curated boundary rebases stored identities to absolute, raw apg query stays stored",
  async () => {
    const bin = binary as string
    const previousBinary = process.env.APG_BINARY
    process.env.APG_BINARY = bin

    const repo = scratchRepo()
    const home = fs.mkdtempSync(path.join(os.tmpdir(), "apg-suite-home-"))
    // Keep `apg init` hermetic: pre-create the plugin dir so it never shells
    // out to npm.
    fs.mkdirSync(path.join(home, ".opencode", "node_modules", "@opencode-ai", "plugin"), {
      recursive: true,
    })
    const env = { ...process.env, HOME: home }
    const run = (args: string[]) =>
      Bun.spawnSync({ cmd: [bin, ...args], cwd: repo, env, stdout: "pipe", stderr: "pipe" })

    try {
      const init = run(["init", "."])
      expect(init.exitCode).toBe(0)
      const scan = run(["scan", "."])
      if (scan.exitCode !== 0) {
        throw new Error(`scan failed: ${scan.stderr.toString()}`)
      }

      const context: ToolContext = { directory: repo, worktree: repo }
      const query = "MATCH (f:File) RETURN f.fqn ORDER BY f.fqn"

      // RAW `apg query` is the graph view: the stored repo-relative form,
      // unchanged (no absolute checkout path).
      const raw = await runCypher(context, query, repo)
      expect(raw).toContain("pkg/a.go")
      expect(raw).not.toContain(repo)

      // A curated tool opts in to rebasing the File-fqn column: the stored
      // identity resolves to an existing absolute path under the caller's
      // project directory.
      const rebased = await runCypher(context, query, repo, { rebaseColumns: [0] })
      const rows = csvToRows(rebased)
      const stored = rows[1][0]
      const resolved = resolveProjectPath(repo, stored)
      expect(resolved).toBe(path.join(repo, "pkg/a.go"))
      expect(existsSync(resolved)).toBe(true)
      // The same stored identity resolves under a DIFFERENT checkout root to a
      // different absolute path (the two checkouts at one commit).
      const other = path.join(path.dirname(repo), "other-checkout")
      expect(resolveProjectPath(other, stored)).toBe(path.join(other, "pkg/a.go"))

      // An absolute input maps back to the stored value the query matched.
      expect(resolveProjectPath(repo, path.join(repo, "pkg/a.go"))).toBe(stored)
    } finally {
      if (previousBinary === undefined) delete process.env.APG_BINARY
      else process.env.APG_BINARY = previousBinary
      fs.rmSync(path.dirname(repo), { recursive: true, force: true })
      fs.rmSync(home, { recursive: true, force: true })
    }
  },
)
