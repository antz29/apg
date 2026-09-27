import { tool } from "@opencode-ai/plugin"
import fs from "node:fs"
import path from "node:path"
import { agentFsGlobs, fsScopeDecision } from "../lib/apg.ts"

export default tool({
  description:
    "Remove filesystem path(s) inside the acting agent's granted globs. Resolves each path against the caller's project `directory`, then SELF-ENFORCES scope: any path not matched by the acting agent's granted globs, or escaping the project/worktree boundary, is refused and NOTHING is removed. All paths are checked before any removal, then a plain fs unlink (no git). Set `recursive` to remove a directory tree.",
  args: {
    directory: tool.schema
      .string()
      .optional()
      .describe("Project root directory to resolve paths against. Defaults to the session's directory."),
    paths: tool.schema
      .array(tool.schema.string())
      .describe("One or more paths (absolute, or relative to `directory`) to remove."),
    recursive: tool.schema
      .boolean()
      .optional()
      .describe("Remove directories and their contents recursively (default false)."),
  },
  async execute(args, context) {
    const paths = args.paths ?? []
    if (paths.length === 0) return "Error: at least one path is required"
    const root = args.directory || context.directory
    const granted = agentFsGlobs(context, root)

    const resolved = paths.map((p) => (path.isAbsolute(p) ? path.resolve(p) : path.resolve(root, p)))
    for (const abs of resolved) {
      const decision = fsScopeDecision(root, abs, granted)
      if (!decision.allowed) return `Refused: ${abs} — ${decision.reason}`
    }

    try {
      for (const abs of resolved) fs.rmSync(abs, { recursive: args.recursive ?? false })
    } catch (e) {
      return `Error: ${(e as Error).message}`
    }
    return `Removed ${resolved.length} path(s):\n${resolved.join("\n")}`
  },
})
