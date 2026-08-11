import type { Plugin } from "@opencode-ai/plugin"
import { tool } from "@opencode-ai/plugin"

export const LadybugScanPlugin: Plugin = async ({ $, worktree }) => {
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
          const dir = args.directory ?? context.worktree
          const scanner = `${context.worktree}/target/release/java_apg`

          const exists = await $`test -f ${scanner}`.nothrow()
          if (exists.exitCode !== 0) {
            return `Error: scanner binary not found at ${scanner}. Build it with: cargo build --release`
          }

          const cmdArgs: string[] = [scanner]

          if (args.language) {
            cmdArgs.push("--language", args.language)
          }

          cmdArgs.push(dir)

          for (const pat of (args.excludePath ?? "").split(",").filter(Boolean)) {
            cmdArgs.push("--exclude-path", pat)
          }

          for (const prefix of (args.blacklist ?? "").split(",").filter(Boolean)) {
            cmdArgs.push(prefix)
          }

          const result = await $`${cmdArgs.map(String)}`.nothrow()
          const output = result.text()
          const stderr = result.stderr.toString()

          if (result.exitCode !== 0) {
            return `Scan failed (exit code ${result.exitCode}):\n${stderr}\n${output}`
          }

          return stderr || "Scan completed successfully."
        },
      }),
    },
  }
}
