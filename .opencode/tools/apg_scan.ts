import { existsSync, statSync } from "node:fs"
import path from "node:path"
import { tool } from "@opencode-ai/plugin"

function resolveProjectRoot(context: { directory: string; worktree: string }): string | null {
  const candidates = [
    context.directory,
    process.cwd(),
    context.worktree,
    path.resolve(import.meta.dir, "..", ".."),
  ]
  for (const c of candidates) {
    if (!c) continue
    if (
      existsSync(path.join(c, "target", "debug", "java_apg")) ||
      existsSync(path.join(c, "target", "release", "java_apg"))
    ) {
      return c
    }
  }
  return null
}

/** Picks the most recently built scanner binary (debug or release). */
function pickBinary(root: string): string | null {
  const candidates = [
    path.join(root, "target", "debug", "java_apg"),
    path.join(root, "target", "release", "java_apg"),
  ]
  let best: string | null = null
  let bestMtime = 0
  for (const c of candidates) {
    if (!existsSync(c)) continue
    const mtime = statSync(c).mtimeMs
    if (mtime > bestMtime) {
      best = c
      bestMtime = mtime
    }
  }
  return best
}

export default tool({
  description:
    "Rebuild the LadybugDB graph database (db.lbug) and graph.jsonl by running the apg scanner + ingestor pipeline on a project (Java, Go, or C++). Run this when source files have changed and the graph is stale.",
  args: {
    directory: tool.schema
      .string()
      .optional()
      .describe("Project root directory to scan. Defaults to the workspace root."),
    language: tool.schema
      .string()
      .optional()
      .describe("Language to scan: java, go, or cpp. Auto-detected if omitted."),
    blacklist: tool.schema
      .string()
      .optional()
      .describe("Comma-separated list of FQN prefixes to exclude from the graph."),
    excludePath: tool.schema
      .string()
      .optional()
      .describe("Comma-separated glob patterns for paths to exclude."),
    modules: tool.schema
      .string()
      .optional()
      .describe("Comma-separated module dirs to scan (Go/C++). Restricts scanning to these modules; defaults to auto-discovery."),
  },
  async execute(args, context) {
    const root = resolveProjectRoot(context)
    const scanner = root ? pickBinary(root) : null
    if (!root || !scanner) {
      return `Error: scanner binary not found. Tried roots:\n  ${[
        context.directory,
        process.cwd(),
        context.worktree,
        path.resolve(import.meta.dir, "..", ".."),
      ]
        .filter(Boolean)
        .join("\n  ")}\nBuild it with: cargo build`
    }

    const dir = args.directory ?? root
    const spawnArgs: string[] = [scanner, dir]

    if (args.language) {
      spawnArgs.push("--language", args.language)
    }
    for (const pat of (args.excludePath ?? "").split(",").filter(Boolean)) {
      spawnArgs.push("--exclude-path", pat)
    }
    for (const m of (args.modules ?? "").split(",").filter(Boolean)) {
      spawnArgs.push("--module", m)
    }
    for (const prefix of (args.blacklist ?? "").split(",").filter(Boolean)) {
      spawnArgs.push(prefix)
    }

    // Run from the project root so db.lbug / graph.jsonl / apg-frontend.log
    // land where ladybug_query and the rest of the tooling expect them.
    const proc = Bun.spawn(spawnArgs, {
      stdout: "pipe",
      stderr: "pipe",
      cwd: root,
    })
    // Drain stdout and stderr concurrently — reading one to completion first
    // can deadlock if the other's pipe buffer fills and blocks the child.
    const [output, stderr] = await Promise.all([
      new Response(proc.stdout).text(),
      new Response(proc.stderr).text(),
    ])
    const exitCode = await proc.exited

    if (exitCode !== 0) {
      return `Scan failed (exit code ${exitCode}):\n${stderr.slice(-4000)}\n${output.slice(-2000)}`
    }

    // stderr carries progress lines and the graph/cleanup counts. Surface the
    // summary lines only, not the per-file progress noise.
    const summary = stderr
      .split("\n")
      .map((l) => l.replace(/\r/g, "").trim())
      .filter((l) => /^(graph:|cleanup:|WARNING:|Skipped|Project:|Language:|\[load\])/.test(l))
      .join("\n")
    return summary || "Scan completed successfully."
  },
})
