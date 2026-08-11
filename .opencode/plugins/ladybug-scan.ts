import { existsSync } from "node:fs"
import path from "node:path"
import type { Plugin } from "@opencode-ai/plugin"
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
    if (existsSync(path.join(c, "target", "release", "java_apg"))) {
      return c
    }
  }
  return null
}

export const LadybugScanPlugin: Plugin = async () => {
  return {
    tool: {
      ladybug_scan: tool({
        description:
          "Rebuild the LadybugDB graph database (db.lbug) by re-scanning a project. Run this when source files have changed and the graph is stale.",
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
        },
        async execute(args, context) {
          const root = resolveProjectRoot(context)
          if (!root) {
            return `Error: scanner binary not found. Tried roots:\n  ${[
              context.directory,
              process.cwd(),
              context.worktree,
              path.resolve(import.meta.dir, "..", ".."),
            ]
              .filter(Boolean)
              .join("\n  ")}\nBuild it with: cargo build --release`
          }

          const dir = args.directory ?? root
          const candidates = [
            path.join(root, "target", "release", "java_apg"),
            path.join(root, "target", "debug", "java_apg"),
          ]

          let scanner = ""
          for (const c of candidates) {
            if (existsSync(c)) {
              scanner = c
              break
            }
          }

          if (!scanner) {
            return `Error: scanner binary not found. Checked:\n  ${candidates.join("\n  ")}\nBuild it with: cargo build --release`
          }

          const spawnArgs: string[] = [scanner, dir]

          if (args.language) {
            spawnArgs.splice(1, 0, "--language", args.language)
          }

          for (const pat of (args.excludePath ?? "").split(",").filter(Boolean)) {
            spawnArgs.push("--exclude-path", pat)
          }

          for (const prefix of (args.blacklist ?? "").split(",").filter(Boolean)) {
            spawnArgs.push(prefix)
          }

          const proc = Bun.spawn(spawnArgs, {
            stdout: "pipe",
            stderr: "pipe",
            cwd: root,
          })
          const output = await new Response(proc.stdout).text()
          const stderr = await new Response(proc.stderr).text()
          const exitCode = await proc.exited

          if (exitCode !== 0) {
            return `Scan failed (exit code ${exitCode}):\n${stderr}\n${output.slice(-2000)}`
          }

          return stderr || "Scan completed successfully."
        },
      }),
    },
  }
}
