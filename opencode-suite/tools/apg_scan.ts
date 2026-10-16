import { Plugin } from "@opencode/plugin"
import { existsSync } from "node:fs"
import path from "node:path"
import { apgBinary, type ToolContext } from "../lib/apg.ts"

function findProjectRoot(context: ToolContext): string | null {
  const candidates = [
    context.location?.directory,
    context.location?.project?.directory,
    context.location?.project?.canonical,
    process.cwd(),
  ]
  for (const c of candidates) {
    if (!c) continue
    if (existsSync(path.join(c, "apg", ".trans")) || existsSync(path.join(c, "apg", "config.json"))) {
      return c
    }
  }
  return null
}

export default Plugin.define({
  id: "apg_scan",
  async setup(ctx) {
    await ctx.tool.transform((editor) => {
      editor.add({
        name: "apg_scan",
        description:
          "Rebuild the project's LadybugDB graph database (apg/.trans/db.lbug + apg/.trans/graph.jsonl) by running the apg scanner + ingestor pipeline. Run this when source files have changed and the graph is stale.",
        input: {
          type: "object",
          properties: {
            directory: {
              type: "string",
              description: "Project root directory to scan. Defaults to the workspace root.",
            },
            language: {
              type: "string",
              description: "Language(s) to scan: java, go, cpp, rust, ts, or csharp. Comma-separated for a multi-language repo (e.g. \"go,ts\"); auto-detected for every language present if omitted.",
            },
            blacklist: {
              type: "string",
              description: "Comma-separated list of FQN prefixes to exclude from the graph.",
            },
            excludePath: {
              type: "string",
              description: "Comma-separated glob patterns for paths to exclude.",
            },
            modules: {
              type: "string",
              description: "Comma-separated module dirs to scan (Go/C++). Restricts scanning to these modules; defaults to auto-discovery.",
            },
          },
          required: [],
          additionalProperties: false,
        },
        async execute(input, context) {
          const merged: ToolContext = {
            agent: context.agent,
            sessionID: context.sessionID,
            messageID: context.messageID,
            id: context.id,
            location: ctx.location,
          }
          const root = findProjectRoot(merged) ?? input.directory
          const dir = input.directory ?? root

          const spawnArgs: string[] = [apgBinary(), "scan", dir]

          if (input.language) {
            spawnArgs.push("--language", input.language)
          }
          for (const pat of (input.excludePath ?? "").split(",").filter(Boolean)) {
            spawnArgs.push("--exclude-path", pat)
          }
          for (const m of (input.modules ?? "").split(",").filter(Boolean)) {
            spawnArgs.push("--module", m)
          }
          for (const prefix of (input.blacklist ?? "").split(",").filter(Boolean)) {
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
            return { content: `Scan failed (exit code ${exitCode}):\n${stderr.slice(-4000)}\n${output.slice(-2000)}` }
          }

          // stderr carries progress lines and the graph/cleanup counts. Surface the
          // summary lines only, not the per-file progress noise.
          const summary = stderr
            .split("\n")
            .map((l) => l.replace(/\r/g, "").trim())
            .filter((l) => /^(graph:|cleanup:|WARNING:|Skipped|Project:|Language:|\[load\])/.test(l))
            .join("\n")
          return { content: summary || "Scan completed successfully." }
        },
      })
    })
  },
})
