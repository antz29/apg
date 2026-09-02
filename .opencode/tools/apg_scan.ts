import { existsSync } from "node:fs"
import path from "node:path"
import { tool } from "@opencode-ai/plugin"

function findProjectRoot(context: { directory: string; worktree: string }): string | null {
  const candidates = [context.directory, process.cwd(), context.worktree]
  for (const c of candidates) {
    if (!c) continue
    if (existsSync(path.join(c, ".apg"))) {
      return c
    }
  }
  return null
}

export default tool({
  description:
    "Rebuild the project's LadybugDB graph database (.apg/db.lbug + .apg/graph.jsonl) by running the apg scanner + ingestor pipeline. Run this when source files have changed and the graph is stale.",
  args: {
    directory: tool.schema
      .string()
      .optional()
      .describe("Project root directory to scan. Defaults to the workspace root."),
    language: tool.schema
      .string()
      .optional()
      .describe("Language(s) to scan: java, go, cpp, rust, or ts. Comma-separated for a multi-language repo (e.g. \"go,ts\"); auto-detected for every language present if omitted."),
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
    const root = findProjectRoot(context) ?? context.directory
    const dir = args.directory ?? root

    const spawnArgs: string[] = ["apg", "scan", dir]

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

    const proc = Bun.spawn(spawnArgs, {
      stdout: "pipe",
      stderr: "pipe",
      cwd: root,
    })
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
